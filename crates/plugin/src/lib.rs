//! API de plugins do Arandu.
//!
//! Plugins são arquivos `.lua` executados em um estado Luau isolado por
//! plugin. O ambiente é sandbox: apenas as bibliotecas seguras de tabela,
//! string, matemática e corrotina são expostas — sem `io`, `os`, `debug`,
//! `package` ou `require`. A única API externa é a tabela global `arandu`.
//!
//! ```lua
//! arandu.register_command("dizer_ola", function(ctx)
//!     arandu.notify("Olá de " .. ctx.current_dir)
//! end)
//! ```
//!
//! Uma falha em um plugin nunca aborta o carregamento dos demais: o erro é
//! registrado em [`PluginManager::errors`].

mod error;

pub use error::PluginError;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mlua::{Function, Lua, LuaOptions, StdLib, Table, VmState};

/// Máximo de pontos de interrupção (chamadas de função/iterações de laço) que
/// um plugin pode executar antes de ser abortado.
const MAX_INTERRUPTS: u64 = 5_000_000;
/// Limite de memória por estado Lua (64 MiB).
const MEMORY_LIMIT: usize = 64 * 1024 * 1024;

/// Contexto entregue a um comando de plugin no momento da execução.
#[derive(Debug, Clone, Default)]
pub struct PluginContext {
    /// Caminhos selecionados (ou o item focado) no painel ativo.
    pub paths: Vec<PathBuf>,
    /// Diretório atual do painel ativo.
    pub current_dir: PathBuf,
}

/// Diretório padrão de plugins: `<config>/arandu/plugins` no Linux
/// (`~/.config/arandu/plugins`), resolvido via [`directories`].
pub fn default_plugin_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("io", "arandu", "arandu")
        .map(|dirs| dirs.config_dir().join("plugins"))
}

/// Um plugin carregado, com seu estado Lua próprio.
struct Plugin {
    lua: Lua,
    commands: HashMap<String, Function>,
    /// Contador de interrupções do estado atual; zerado antes de cada execução.
    interrupts: Arc<AtomicU64>,
}

/// Gerenciador de plugins carregados.
pub struct PluginManager {
    plugins: Vec<Plugin>,
    /// Nome do comando -> índice do plugin que o registrou (último vence).
    command_index: HashMap<String, usize>,
    /// Falhas de carregamento: `(nome do arquivo/plugin, erro)`.
    pub errors: Vec<(String, PluginError)>,
    /// Mensagens de `arandu.notify` produzidas na última execução.
    output: Vec<String>,
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::empty()
    }
}

impl PluginManager {
    /// Gerenciador vazio (nenhum plugin carregado).
    pub fn empty() -> Self {
        Self {
            plugins: Vec::new(),
            command_index: HashMap::new(),
            errors: Vec::new(),
            output: Vec::new(),
        }
    }

    /// Carrega o diretório padrão de plugins, se existir.
    pub fn load_default() -> Self {
        match default_plugin_dir() {
            Some(dir) => Self::load_dir(&dir),
            None => Self::empty(),
        }
    }

    /// Carrega cada `*.lua` de `path` em um estado Luau isolado.
    ///
    /// Diretório inexistente resulta em um gerenciador vazio. Erros de um
    /// plugin são coletados em [`PluginManager::errors`] sem impedir o
    /// carregamento dos demais.
    pub fn load_dir(path: &Path) -> Self {
        let mut manager = Self::empty();
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return manager,
            Err(error) => {
                manager
                    .errors
                    .push((path.display().to_string(), PluginError::Io(error)));
                return manager;
            }
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|file| file.extension().is_some_and(|ext| ext == "lua"))
            .collect();
        files.sort();

        for file in files {
            let name = file
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.display().to_string());
            match Plugin::load(&name, &file) {
                Ok(plugin) => manager.insert(plugin),
                Err(error) => manager.errors.push((name, error)),
            }
        }
        manager
    }

    fn insert(&mut self, plugin: Plugin) {
        let index = self.plugins.len();
        for name in plugin.commands.keys() {
            self.command_index.insert(name.clone(), index);
        }
        self.plugins.push(plugin);
    }

    /// Nomes de todos os comandos registrados, em ordem alfabética.
    pub fn commands(&self) -> Vec<String> {
        let mut names: Vec<String> = self.command_index.keys().cloned().collect();
        names.sort();
        names
    }

    /// Executa um comando registrado com o contexto informado.
    ///
    /// As mensagens de `arandu.notify` ficam disponíveis em
    /// [`PluginManager::take_output`] após a execução.
    pub fn run_command(&mut self, name: &str, ctx: &PluginContext) -> Result<(), PluginError> {
        let index = *self
            .command_index
            .get(name)
            .ok_or_else(|| PluginError::UnknownCommand(name.to_owned()))?;
        let plugin = &self.plugins[index];
        let callback = plugin
            .commands
            .get(name)
            .ok_or_else(|| PluginError::UnknownCommand(name.to_owned()))?;

        self.output.clear();
        plugin.run(callback, ctx)?;
        self.output = plugin.output()?;
        Ok(())
    }

    /// Mensagens de `notify` produzidas na última execução.
    pub fn output(&self) -> &[String] {
        &self.output
    }

    /// Retira as mensagens da última execução, deixando o buffer vazio.
    pub fn take_output(&mut self) -> Vec<String> {
        std::mem::take(&mut self.output)
    }
}

impl Plugin {
    /// Lê e executa o script, coletando os comandos registrados.
    fn load(name: &str, path: &Path) -> Result<Self, PluginError> {
        let source = std::fs::read_to_string(path)?;

        // O backend Luau não possui `io`/`package`; restam `os` e `debug` para
        // excluir do conjunto seguro (XOR remove as flags, todas ligadas em
        // `ALL_SAFE`).
        let libs = StdLib::ALL_SAFE ^ (StdLib::OS | StdLib::DEBUG);
        let lua = Lua::new_with(libs, LuaOptions::default())?;
        lua.set_memory_limit(MEMORY_LIMIT)?;

        // Aborta o plugin se ele ultrapassar o orçamento de execução. O
        // `set_interrupt` do Luau é chamado em cada chamada de função e
        // iteração de laço, o que cobre tanto scripts infinitos no topo
        // (`ensure_plugins`) quanto dentro de comandos.
        let interrupts = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&interrupts);
        lua.set_interrupt(move |_| {
            if counter.fetch_add(1, Ordering::Relaxed) + 1 > MAX_INTERRUPTS {
                Err(mlua::Error::runtime(
                    "plugin excedeu limite de execução (possível laço infinito)",
                ))
            } else {
                Ok(VmState::Continue)
            }
        });

        lua.set_named_registry_value("arandu.commands", lua.create_table()?)?;
        lua.set_named_registry_value("arandu.output", lua.create_table()?)?;
        let ctx = lua.create_table()?;
        ctx.set("paths", lua.create_table()?)?;
        ctx.set("current_dir", String::new())?;
        lua.set_named_registry_value("arandu.ctx", ctx)?;

        let arandu = lua.create_table()?;
        arandu.set(
            "register_command",
            lua.create_function(|lua, (command, callback): (String, Function)| {
                if command.trim().is_empty() {
                    return Err(mlua::Error::runtime("nome de comando vazio"));
                }
                let commands: Table = lua.named_registry_value("arandu.commands")?;
                commands.set(command, callback)?;
                Ok(())
            })?,
        )?;
        arandu.set(
            "notify",
            lua.create_function(|lua, message: String| {
                let output: Table = lua.named_registry_value("arandu.output")?;
                let index = output.raw_len() + 1;
                output.set(index, message)?;
                Ok(())
            })?,
        )?;
        arandu.set(
            "selected_paths",
            lua.create_function(|lua, ()| {
                let ctx: Table = lua.named_registry_value("arandu.ctx")?;
                ctx.get::<Table>("paths")
            })?,
        )?;
        arandu.set(
            "current_dir",
            lua.create_function(|lua, ()| {
                let ctx: Table = lua.named_registry_value("arandu.ctx")?;
                ctx.get::<String>("current_dir")
            })?,
        )?;
        lua.globals().set("arandu", arandu)?;

        interrupts.store(0, Ordering::Relaxed);
        lua.load(&source).set_name(name).exec()?;

        let registered: Table = lua.named_registry_value("arandu.commands")?;
        let mut commands = HashMap::new();
        for pair in registered.pairs::<String, Function>() {
            let (command, callback) = pair?;
            commands.insert(command, callback);
        }

        Ok(Self {
            lua,
            commands,
            interrupts,
        })
    }

    /// Injeta o contexto da execução e zera o buffer de saída.
    fn set_context(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let paths = self.lua.create_table()?;
        for (index, path) in ctx.paths.iter().enumerate() {
            paths.set(index + 1, path.to_string_lossy().into_owned())?;
        }
        let table = self.lua.create_table()?;
        table.set("paths", paths)?;
        table.set(
            "current_dir",
            ctx.current_dir.to_string_lossy().into_owned(),
        )?;
        self.lua.set_named_registry_value("arandu.ctx", table)?;
        self.lua
            .set_named_registry_value("arandu.output", self.lua.create_table()?)?;
        Ok(())
    }

    fn run(&self, callback: &Function, ctx: &PluginContext) -> Result<(), PluginError> {
        self.set_context(ctx)?;
        let context: Table = self.lua.named_registry_value("arandu.ctx")?;
        self.interrupts.store(0, Ordering::Relaxed);
        callback.call::<()>(context)?;
        Ok(())
    }

    fn output(&self) -> Result<Vec<String>, PluginError> {
        let output: Table = self.lua.named_registry_value("arandu.output")?;
        let mut messages = Vec::new();
        for message in output.sequence_values::<String>() {
            messages.push(message?);
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::{PluginContext, PluginManager};
    use std::path::PathBuf;

    fn write_plugin(dir: &std::path::Path, name: &str, source: &str) {
        std::fs::write(dir.join(name), source).expect("escreve plugin");
    }

    #[test]
    fn loads_and_runs_command_with_context() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        write_plugin(
            dir.path(),
            "ola.lua",
            r#"
            arandu.register_command("ola", function(ctx)
                local primeiro = ctx.paths[1] or "(nenhum)"
                arandu.notify("ctx=" .. ctx.current_dir .. "|" .. primeiro)
                arandu.notify("selecionados=" .. #arandu.selected_paths())
            end)
            "#,
        );

        let mut manager = PluginManager::load_dir(dir.path());
        assert!(manager.errors.is_empty(), "erros: {:?}", manager.errors);
        assert_eq!(manager.commands(), vec!["ola".to_owned()]);

        let ctx = PluginContext {
            paths: vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")],
            current_dir: PathBuf::from("/tmp"),
        };
        manager.run_command("ola", &ctx).expect("executa comando");
        assert_eq!(
            manager.output(),
            &[
                "ctx=/tmp|/tmp/a.txt".to_owned(),
                "selecionados=2".to_owned(),
            ]
        );
        assert!(manager.take_output().len() == 2);
        assert!(manager.output().is_empty());
    }

    #[test]
    fn syntax_error_is_collected_and_other_plugins_still_load() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        write_plugin(dir.path(), "quebrado.lua", "arandu.register_command(");
        write_plugin(
            dir.path(),
            "valido.lua",
            r#"arandu.register_command("ok", function() end)"#,
        );

        let manager = PluginManager::load_dir(dir.path());
        assert_eq!(manager.commands(), vec!["ok".to_owned()]);
        assert_eq!(manager.errors.len(), 1);
        assert_eq!(manager.errors[0].0, "quebrado");
    }

    #[test]
    fn sandbox_hides_io_and_os() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        write_plugin(
            dir.path(),
            "io.lua",
            r#"local f = io.open("/etc/passwd", "r")"#,
        );
        write_plugin(
            dir.path(),
            "os.lua",
            r#"arandu.register_command("sair", function() os.exit(0) end)"#,
        );

        let mut manager = PluginManager::load_dir(dir.path());
        // `io.open` no topo do script falha no carregamento.
        assert!(
            manager.errors.iter().any(|(name, _)| name == "io"),
            "erros: {:?}",
            manager.errors
        );
        // `os.exit` dentro do comando falha em tempo de execução.
        let ctx = PluginContext::default();
        manager
            .run_command("sair", &ctx)
            .expect_err("os.exit deve falhar no sandbox");
    }

    #[test]
    fn commands_lists_all_registered_names() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        write_plugin(
            dir.path(),
            "dois.lua",
            r#"
            arandu.register_command("beta", function() end)
            arandu.register_command("alfa", function() end)
            "#,
        );

        let manager = PluginManager::load_dir(dir.path());
        assert_eq!(
            manager.commands(),
            vec!["alfa".to_owned(), "beta".to_owned()]
        );
    }

    #[test]
    fn infinite_loop_is_aborted_by_execution_limit() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        write_plugin(
            dir.path(),
            "loop.lua",
            r#"arandu.register_command("trava", function()
                   while true do end
               end)"#,
        );

        let mut manager = PluginManager::load_dir(dir.path());
        assert!(manager.errors.is_empty(), "erros: {:?}", manager.errors);

        let started = std::time::Instant::now();
        let error = manager
            .run_command("trava", &PluginContext::default())
            .expect_err("laço infinito deveria ser abortado");
        let message = error.to_string();
        assert!(
            message.contains("excedeu limite"),
            "mensagem inesperada: {message}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "limite demorou demais para disparar: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn infinite_loop_at_top_level_is_aborted_on_load() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        write_plugin(dir.path(), "topo.lua", "while true do end");

        let manager = PluginManager::load_dir(dir.path());
        assert_eq!(manager.errors.len(), 1, "erros: {:?}", manager.errors);
        assert!(
            manager.errors[0].1.to_string().contains("excedeu limite"),
            "erro inesperado: {:?}",
            manager.errors[0].1
        );
    }

    #[test]
    fn missing_directory_yields_empty_manager() {
        let manager = PluginManager::load_dir(std::path::Path::new("/nao/existe/arandu"));
        assert!(manager.errors.is_empty());
        assert!(manager.commands().is_empty());
    }
}
