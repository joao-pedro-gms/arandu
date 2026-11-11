//! Integração Git do Arandu.
//!
//! Este crate expõe a primeira fatia da integração Git: descoberta de
//! repositório, leitura de status, staging/unstaging e criação de commits.
//! Quase tudo é implementado com [`gix`] (gitoxide); a exceção é o stash, que
//! usa o binário `git` em tempo de execução (ver abaixo).
//!
//! # Decisões e trade-offs
//!
//! - O status é obtido com [`gix::Repository::status`], que já entrega tanto as
//!   mudanças `HEAD -> index` (staged) quanto `index -> worktree` (modified)
//!   e arquivos não rastreados. Renomeações no worktree dependem de detecção
//!   de similaridade e não são habilitadas aqui; renomeações no index são
//!   reportadas como [`FileStatus::Renamed`] quando o gix as detecta.
//! - O staging é feito manualmente: o blob é gravado no object database, uma
//!   [`gix::index::Entry`] é montada e inserida no index, que então é escrito.
//!   Isso evita depender de um "add path" de alto nível, que o gix 0.90 não
//!   oferece.
//! - O commit precisa de uma árvore. Como o gix não expõe um "write tree from
//!   index" pronto, as árvores são montadas recursivamente a partir das
//!   entradas do index e gravadas com [`gix::Repository::write_object`].
//! - `ahead`/`behind` são calculados caminhando os ancestrais de `HEAD` e do
//!   upstream até um limite de segurança (10.000 commits por lado). Em
//!   repositórios gigantes o valor pode ser saturado; isso é documentado na
//!   struct [`GitStatus`].
//! - A identidade do commit vem de `user.name`/`user.email` (via gix). Se
//!   ausente, usamos `Arandu <arandu@localhost>`, como combinado.
//! - O log é um walk first-DFS a partir de `HEAD` (primeiro pai primeiro),
//!   limitado por [`DEFAULT_LOG_LIMIT`]. O grafo ASCII é uma aproximação
//!   determinística de `git log --graph --oneline`: uma linha por commit,
//!   com colunas de lanes e um marcador `|\` nos merges.
//! - O blame usa o algoritmo nativo do `gix-blame` (já habilitado pela feature
//!   `extras` padrão do gix 0.90), partindo do commit de `HEAD`. Linhas
//!   alteradas apenas no worktree não aparecem: o blame reflete o conteúdo
//!   commitado, não o arquivo em disco. Isso mantém a operação barata e
//!   determinística.
//! - Branches são listadas e manipuladas com o ref store do gix. A listagem
//!   cobre apenas branches locais (`refs/heads/`): incluir refs remote-tracking
//!   exigiria decidir se o checkout cria ou não uma branch local de
//!   acompanhamento, e essa ambiguidade não cabe na interface atual.
//! - Stash é a única operação que recorre ao binário `git`. O gix 0.90 não
//!   expõe API de stash, e o formato envolve commits de worktree/index e um
//!   reflog próprio cuja reaplicação é um merge — reimplementar isso seria
//!   arriscado e incompatível com `git stash`. Usar o binário mantém os stashes
//!   interoperáveis com o Git; o restante do crate continua 100% gix.
//! - Datas são formatadas em `YYYY-MM-DD HH:MM` no fuso gravado no commit
//!   (o mesmo que `git log --date=local` mostraria para o autor).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::Mode as IndexMode;
use gix::objs::tree::{EntryKind, EntryMode};

/// Máximo de commits visitados ao calcular `ahead`/`behind` de cada lado.
const MAX_GRAPH_WALK: usize = 10_000;

/// Limite padrão de commits retornados por [`Repo::log`].
pub const DEFAULT_LOG_LIMIT: usize = 500;

/// Limite padrão de linhas anotadas por [`Repo::blame_file`].
pub const DEFAULT_BLAME_LINES: usize = 2_000;

/// Identidade usada quando o repositório não tem `user.name`/`user.email`.
const FALLBACK_NAME: &str = "Arandu";
const FALLBACK_EMAIL: &str = "arandu@localhost";

/// Erros da integração Git.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// O caminho não pertence a um repositório Git.
    #[error("não é um repositório git")]
    NotARepository,
    /// O repositório não possui worktree (repositório bare).
    #[error("repositório sem worktree")]
    NoWorktree,
    /// Um caminho não está dentro do worktree.
    #[error("caminho fora do repositório: {0}")]
    PathOutsideRepository(PathBuf),
    /// Um caminho inválido foi informado (por exemplo, o próprio root).
    #[error("caminho inválido: {0}")]
    InvalidPath(PathBuf),
    /// A mensagem de commit está vazia.
    #[error("mensagem de commit vazia")]
    EmptyMessage,
    /// Não há mudanças staged para commitar.
    #[error("nada staged para commitar")]
    NothingStaged,
    /// O nome de branch informado é inválido.
    #[error("nome de branch inválido: {0}")]
    InvalidBranchName(String),
    /// A branch informada não existe.
    #[error("branch não encontrada: {0}")]
    BranchNotFound(String),
    /// Já existe uma branch com o nome informado.
    #[error("branch já existe: {0}")]
    BranchAlreadyExists(String),
    /// A operação não é permitida na branch atual.
    #[error("não é possível excluir a branch atual: {0}")]
    CurrentBranch(String),
    /// Há mudanças staged ou modificadas que seriam sobrescritas.
    #[error("há mudanças não commitadas; faça commit ou guarde um stash antes de trocar de branch")]
    DirtyWorktree,
    /// Não há mudanças para guardar no stash.
    #[error("nada para guardar no stash")]
    NothingToStash,
    /// O stash informado não existe.
    #[error("stash não encontrado: {0}")]
    StashNotFound(String),
    /// Alguma operação do gix falhou.
    #[error("erro git: {0}")]
    Gix(String),
    /// Alguma operação de I/O falhou.
    #[error("erro de i/o: {0}")]
    Io(#[from] std::io::Error),
}

fn gix_err(error: impl std::fmt::Display) -> GitError {
    GitError::Gix(error.to_string())
}

/// Tipo de mudança de um arquivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileStatus {
    /// Arquivo adicionado.
    Added,
    /// Arquivo modificado.
    Modified,
    /// Arquivo removido.
    Deleted,
    /// Arquivo renomeado.
    Renamed,
}

/// Um arquivo com seu estado no repositório.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Caminho relativo ao root do worktree.
    pub path: PathBuf,
    /// Estado do arquivo.
    pub status: FileStatus,
}

/// Status consolidado do repositório.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    /// Nome da branch atual, ou `None` se `HEAD` estiver detached.
    pub branch: Option<String>,
    /// Nome do upstream (ex.: `origin/main`), se configurado.
    pub upstream: Option<String>,
    /// Commits à frente do upstream.
    ///
    /// Limitado a [`MAX_GRAPH_WALK`]; em repositórios muito grandes o valor
    /// pode ficar saturado nesse limite.
    pub ahead: usize,
    /// Commits atrás do upstream. Mesma limitação de [`Self::ahead`].
    pub behind: usize,
    /// Arquivos no index diferentes de `HEAD`.
    pub staged: Vec<FileEntry>,
    /// Arquivos rastreados com mudanças no worktree.
    pub modified: Vec<FileEntry>,
    /// Arquivos não rastreados.
    pub untracked: Vec<FileEntry>,
}

impl GitStatus {
    /// `true` quando não há mudanças staged, modificadas ou não rastreadas.
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.modified.is_empty() && self.untracked.is_empty()
    }
}

/// Um commit resumido para o log gráfico.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Id curto (8 caracteres hexadecimais).
    pub id: String,
    /// Primeira linha da mensagem de commit.
    pub message: String,
    /// Nome do autor.
    pub author: String,
    /// Data do commit em `YYYY-MM-DD HH:MM` (fuso do commit).
    pub time: String,
    /// Ids curtos dos pais (2 ou mais indicam merge).
    pub parents: Vec<String>,
}

/// Uma linha de arquivo anotada com o commit que a introduziu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    /// Id curto (8 caracteres hexadecimais) do commit.
    pub commit: String,
    /// Nome do autor do commit.
    pub author: String,
    /// Data do commit em `YYYY-MM-DD HH:MM` (fuso do commit).
    pub time: String,
    /// Conteúdo da linha, sem o terminador.
    pub content: String,
}

/// Uma branch local do repositório.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchInfo {
    /// Nome curto da branch (ex.: `main`).
    pub name: String,
    /// `true` quando é a branch apontada por `HEAD`.
    pub current: bool,
    /// Id curto (8 caracteres hexadecimais) do commit apontado.
    pub commit: String,
}

/// Uma entrada da pilha de stash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashInfo {
    /// Id curto (8 caracteres hexadecimais) do commit de stash.
    pub id: String,
    /// Mensagem descritiva (`git stash list`).
    pub message: String,
    /// Data em `YYYY-MM-DD HH:MM`.
    pub time: String,
}

/// Um repositório Git aberto.
pub struct Repo {
    inner: gix::Repository,
    root: PathBuf,
}

impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo").field("root", &self.root).finish()
    }
}

impl Repo {
    /// Descobre um repositório a partir de `path`, subindo pelos diretórios.
    ///
    /// Retorna `None` se nenhum repositório for encontrado ou se ele for bare.
    pub fn discover(path: &Path) -> Option<Repo> {
        let inner = gix::discover(path).ok()?;
        let root = inner.workdir()?.to_path_buf();
        Some(Repo { inner, root })
    }

    /// Raiz do worktree.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Lê o status atual do repositório.
    pub fn status(&self) -> Result<GitStatus, GitError> {
        let head = self.inner.head().map_err(gix_err)?;
        let branch = head
            .referent_name()
            .map(|name| name.shorten().to_string())
            .filter(|name| name != "HEAD");
        let upstream = self.upstream_name();
        let (ahead, behind) = self.ahead_behind();

        let mut staged = Vec::new();
        let mut modified = Vec::new();
        let mut untracked = Vec::new();

        let iter = self
            .inner
            .status(gix::progress::Discard)
            .map_err(gix_err)?
            .untracked_files(gix::status::UntrackedFiles::Files)
            .into_iter(std::iter::empty::<BString>())
            .map_err(gix_err)?;

        for item in iter {
            let item = item.map_err(gix_err)?;
            match item {
                gix::status::Item::TreeIndex(change) => {
                    staged.extend(tree_index_change(&change));
                }
                gix::status::Item::IndexWorktree(item) => match item {
                    gix::status::index_worktree::Item::Modification {
                        rela_path, status, ..
                    } => {
                        if let Some(status) = entry_status(&status) {
                            modified.push(FileEntry {
                                path: to_path(rela_path.as_ref()),
                                status,
                            });
                        }
                    }
                    gix::status::index_worktree::Item::DirectoryContents { entry, .. } => {
                        if entry.status == gix::dir::entry::Status::Untracked
                            && entry.disk_kind != Some(gix::dir::entry::Kind::Directory)
                        {
                            untracked.push(FileEntry {
                                path: to_path(entry.rela_path.as_ref()),
                                status: FileStatus::Added,
                            });
                        }
                    }
                    gix::status::index_worktree::Item::Rewrite { dirwalk_entry, .. } => {
                        modified.push(FileEntry {
                            path: to_path(dirwalk_entry.rela_path.as_ref()),
                            status: FileStatus::Modified,
                        });
                    }
                },
            }
        }

        sort_dedup(&mut staged);
        sort_dedup(&mut modified);
        sort_dedup(&mut untracked);

        Ok(GitStatus {
            branch,
            upstream,
            ahead,
            behind,
            staged,
            modified,
            untracked,
        })
    }

    /// Adiciona `paths` ao index.
    ///
    /// Arquivos inexistentes no disco são tratados como remoções staged.
    pub fn stage(&self, paths: &[PathBuf]) -> Result<(), GitError> {
        let mut index = self.load_index()?;

        for path in paths {
            let rela = self.rela_path(path)?;
            let worktree_path = self.root.join(path);

            match std::fs::symlink_metadata(&worktree_path) {
                Ok(metadata) if metadata.is_dir() => {
                    return Err(GitError::InvalidPath(path.clone()));
                }
                Ok(metadata) => {
                    let (bytes, mode) = if metadata.file_type().is_symlink() {
                        let target = std::fs::read_link(&worktree_path)?;
                        (
                            target.to_string_lossy().into_owned().into_bytes(),
                            IndexMode::SYMLINK,
                        )
                    } else {
                        let executable = is_executable(&metadata);
                        let mode = if executable {
                            IndexMode::FILE_EXECUTABLE
                        } else {
                            IndexMode::FILE
                        };
                        (std::fs::read(&worktree_path)?, mode)
                    };

                    let id = self.inner.write_blob(&bytes).map_err(gix_err)?.detach();
                    let stat = gix::index::entry::Stat {
                        size: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                        ..Default::default()
                    };

                    index.remove_entries(|_, existing, _| existing == rela.as_bstr());
                    index.dangerously_push_entry(
                        stat,
                        id,
                        gix::index::entry::Flags::empty(),
                        mode,
                        rela.as_bstr(),
                    );
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    index.remove_entries(|_, existing, _| existing == rela.as_bstr());
                }
                Err(err) => return Err(GitError::Io(err)),
            }
        }

        index.sort_entries();
        self.write_index(&mut index)
    }

    /// Remove `paths` do index, restaurando a versão de `HEAD` quando existir.
    pub fn unstage(&self, paths: &[PathBuf]) -> Result<(), GitError> {
        let mut index = self.load_index()?;
        let head = self.head_entries()?;

        for path in paths {
            let rela = self.rela_path(path)?;
            match head.get(rela.as_slice()) {
                Some((id, mode)) => {
                    if let Some(entry) = index.entry_mut_by_path_and_stage(
                        rela.as_bstr(),
                        gix::index::entry::Stage::Unconflicted,
                    ) {
                        entry.id = *id;
                        entry.mode = *mode;
                        entry.stat = gix::index::entry::Stat::default();
                    } else {
                        index.dangerously_push_entry(
                            gix::index::entry::Stat::default(),
                            *id,
                            gix::index::entry::Flags::empty(),
                            *mode,
                            rela.as_bstr(),
                        );
                    }
                }
                None => {
                    index.remove_entries(|_, existing, _| existing == rela.as_bstr());
                }
            }
        }

        index.sort_entries();
        self.write_index(&mut index)
    }

    /// Cria um commit com o index atual e retorna o id do commit.
    ///
    /// Retorna [`GitError::EmptyMessage`] para mensagens vazias e
    /// [`GitError::NothingStaged`] quando a árvore do index é igual à de `HEAD`.
    pub fn commit(&self, message: &str) -> Result<gix::ObjectId, GitError> {
        let message = message.trim();
        if message.is_empty() {
            return Err(GitError::EmptyMessage);
        }

        let index = self.load_index()?;
        let tree = self.write_tree_from_index(&index)?;

        let mut head = self.inner.head().map_err(gix_err)?;
        let parents: Vec<gix::ObjectId> = if head.is_unborn() {
            Vec::new()
        } else {
            let head_commit = head.peel_to_commit().map_err(gix_err)?;
            if head_commit.tree_id().map_err(gix_err)?.detach() == tree {
                return Err(GitError::NothingStaged);
            }
            vec![head_commit.id().detach()]
        };

        let (name, email) = self.identity();
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let time = format!("{seconds} +0000");
        let signature = gix::actor::SignatureRef {
            name: BStr::new(&name),
            email: BStr::new(&email),
            time: time.as_str(),
        };

        self.inner
            .commit_as(signature, signature, "HEAD", message, tree, parents)
            .map_err(gix_err)
            .map(|id| id.detach())
    }

    /// Caminha os commits de `HEAD` em profundidade, primeiro pai primeiro.
    ///
    /// `max == 0` usa [`DEFAULT_LOG_LIMIT`]. Um repositório sem commits
    /// (HEAD unborn) retorna uma lista vazia.
    pub fn log(&self, max: usize) -> Result<Vec<CommitInfo>, GitError> {
        let max = if max == 0 { DEFAULT_LOG_LIMIT } else { max };
        let mut head = self.inner.head().map_err(gix_err)?;
        if head.is_unborn() {
            return Ok(Vec::new());
        }
        let head_id = head.peel_to_commit().map_err(gix_err)?.id().detach();

        let mut commits = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![head_id];
        while let Some(id) = stack.pop() {
            if commits.len() >= max {
                break;
            }
            if !seen.insert(id) {
                continue;
            }
            let Ok(commit) = self.inner.find_commit(id) else {
                continue;
            };
            let author = commit
                .author()
                .ok()
                .map(|author| author.name.to_str_lossy().trim().to_string())
                .unwrap_or_default();
            let time = commit
                .time()
                .ok()
                .map(format_git_time)
                .unwrap_or_else(|| "-".to_string());
            let message = commit
                .message()
                .ok()
                .map(|message| message.title.to_str_lossy().trim().to_string())
                .unwrap_or_default();
            let parents: Vec<gix::ObjectId> =
                commit.parent_ids().map(|parent| parent.detach()).collect();
            // Empilha os pais ao contrário para visitar o primeiro pai primeiro.
            for parent in parents.iter().rev() {
                if !seen.contains(parent) {
                    stack.push(*parent);
                }
            }
            commits.push(CommitInfo {
                id: short_id(id),
                message,
                author,
                time,
                parents: parents.into_iter().map(short_id).collect(),
            });
        }
        Ok(commits)
    }

    /// Anota cada linha de `path` (na versão de `HEAD`) com o commit que a
    /// introduziu.
    ///
    /// `max_lines == 0` usa [`DEFAULT_BLAME_LINES`]. A anotação usa o algoritmo
    /// nativo do `gix-blame`; arquivos sem versão commitada retornam erro.
    pub fn blame_file(&self, path: &Path, max_lines: usize) -> Result<Vec<BlameLine>, GitError> {
        let max_lines = if max_lines == 0 {
            DEFAULT_BLAME_LINES
        } else {
            max_lines
        };
        let rela = self.rela_path(path)?;
        let mut head = self.inner.head().map_err(gix_err)?;
        if head.is_unborn() {
            return Ok(Vec::new());
        }
        let head_id = head.peel_to_commit().map_err(gix_err)?.id().detach();

        let limit = u32::try_from(max_lines).unwrap_or(u32::MAX);
        let range = 0..limit;
        let outcome = self
            .inner
            .blame_file(
                rela.as_bstr(),
                head_id,
                gix::repository::blame_file::Options {
                    ranges: gix::blame::BlameRanges::PartialFile(vec![range]),
                    ..Default::default()
                },
            )
            .map_err(gix_err)?;

        let mut lines = Vec::new();
        for (entry, content) in outcome.entries_with_lines() {
            let (author, time) = self.commit_meta(entry.commit_id);
            let commit = short_id(entry.commit_id);
            for line in content {
                if lines.len() >= max_lines {
                    break;
                }
                let content = line.to_str_lossy();
                let content = content.trim_end_matches(['\n', '\r']).to_string();
                lines.push(BlameLine {
                    commit: commit.clone(),
                    author: author.clone(),
                    time: time.clone(),
                    content,
                });
            }
            if lines.len() >= max_lines {
                break;
            }
        }
        lines.truncate(max_lines);
        Ok(lines)
    }

    /// Lista as branches locais, com a atual marcada.
    ///
    /// Refs remote-tracking não são incluídas; veja a documentação do módulo.
    pub fn branches(&self) -> Result<Vec<BranchInfo>, GitError> {
        let current = self
            .inner
            .head()
            .ok()
            .and_then(|head| head.referent_name().map(|name| name.shorten().to_string()));

        let mut branches = Vec::new();
        let references = self.inner.references().map_err(gix_err)?;
        let iter = references
            .local_branches()
            .map_err(gix_err)?
            .peeled()
            .map_err(gix_err)?;
        for reference in iter {
            let reference = reference.map_err(gix_err)?;
            let Some(id) = reference.try_id() else {
                continue;
            };
            let name = reference.name().shorten().to_string();
            branches.push(BranchInfo {
                current: current.as_deref() == Some(name.as_str()),
                name,
                commit: short_id(id.detach()),
            });
        }
        branches.sort_by(|a, b| b.current.cmp(&a.current).then_with(|| a.name.cmp(&b.name)));
        Ok(branches)
    }

    /// Troca `HEAD` para a branch local `name` e atualiza o worktree.
    ///
    /// Retorna [`GitError::DirtyWorktree`] se houver mudanças staged ou
    /// modificadas; arquivos não rastreados não bloqueiam a troca (como no
    /// `git checkout`, eles permanecem no worktree).
    pub fn checkout(&self, name: &str) -> Result<(), GitError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(GitError::BranchNotFound(name.to_string()));
        }
        if self
            .inner
            .head()
            .ok()
            .and_then(|head| head.referent_name().map(|name| name.shorten().to_string()))
            .as_deref()
            == Some(name)
        {
            return Ok(());
        }

        let status = self.status()?;
        if !status.staged.is_empty() || !status.modified.is_empty() {
            return Err(GitError::DirtyWorktree);
        }

        let full_name = format!("refs/heads/{name}");
        let reference = self
            .inner
            .find_reference(full_name.as_str())
            .map_err(|_| GitError::BranchNotFound(name.to_string()))?;
        let commit_id = reference
            .try_id()
            .ok_or_else(|| GitError::BranchNotFound(name.to_string()))?
            .detach();
        let commit = self.inner.find_commit(commit_id).map_err(gix_err)?;
        let tree_id = commit.tree_id().map_err(gix_err)?.detach();

        let old_index = self.load_index()?;
        let old_paths: Vec<PathBuf> = old_index
            .entries_with_paths_by_filter_map(|path, _| Some(to_path(path)))
            .map(|(_, path)| path)
            .collect();
        let mut index = self.inner.index_from_tree(&tree_id).map_err(gix_err)?;
        let new_paths: HashSet<PathBuf> = index
            .entries_with_paths_by_filter_map(|path, _| Some(to_path(path)))
            .map(|(_, path)| path)
            .collect();

        // Arquivos rastreados que não existem na árvore de destino precisam
        // sair do disco: o checkout de baixo nível só escreve o que está no
        // index.
        for path in old_paths {
            if new_paths.contains(&path) {
                continue;
            }
            match std::fs::remove_file(self.root.join(&path)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(GitError::Io(error)),
            }
        }

        let mut options = self
            .inner
            .checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)
            .map_err(gix_err)?;
        options.overwrite_existing = true;
        let should_interrupt = AtomicBool::new(false);
        gix::worktree::state::checkout(
            &mut index,
            self.root.clone(),
            self.inner.objects.clone(),
            &gix::progress::Discard,
            &gix::progress::Discard,
            &should_interrupt,
            options,
        )
        .map_err(gix_err)?;

        self.write_index(&mut index)?;

        use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
        let head_name = gix::refs::FullName::try_from("HEAD")
            .map_err(|error| GitError::Gix(error.to_string()))?;
        let target = gix::refs::FullName::try_from(full_name)
            .map_err(|error| GitError::InvalidBranchName(error.to_string()))?;
        self.inner
            .edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!("checkout: moving to {name}").into(),
                    },
                    expected: PreviousValue::Any,
                    new: gix::refs::Target::Symbolic(target),
                },
                name: head_name,
                deref: false,
            })
            .map_err(gix_err)?;
        Ok(())
    }

    /// Cria uma branch local apontando para o commit atual de `HEAD`.
    ///
    /// A branch criada não é ativada; use [`Repo::checkout`] em seguida.
    pub fn create_branch(&self, name: &str) -> Result<(), GitError> {
        let name = validate_branch_name(name)?;
        let full_name = format!("refs/heads/{name}");
        if self.inner.find_reference(full_name.as_str()).is_ok() {
            return Err(GitError::BranchAlreadyExists(name.to_string()));
        }
        let mut head = self.inner.head().map_err(gix_err)?;
        if head.is_unborn() {
            return Err(GitError::Gix(
                "não há commit em HEAD para apontar a nova branch".into(),
            ));
        }
        let commit = head.peel_to_commit().map_err(gix_err)?.id().detach();

        use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
        let ref_name = gix::refs::FullName::try_from(full_name)
            .map_err(|error| GitError::InvalidBranchName(error.to_string()))?;
        self.inner
            .edit_reference(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: false,
                        message: format!("branch: created from HEAD ({name})").into(),
                    },
                    expected: PreviousValue::MustNotExist,
                    new: gix::refs::Target::Object(commit),
                },
                name: ref_name,
                deref: false,
            })
            .map_err(gix_err)?;
        Ok(())
    }

    /// Exclui a branch local `name`.
    ///
    /// Retorna [`GitError::CurrentBranch`] se for a branch apontada por
    /// `HEAD`; a exclusão de branches com commits não mesclados é permitida
    /// (equivalente a `git branch -D`), pois a interface pede confirmação.
    pub fn delete_branch(&self, name: &str) -> Result<(), GitError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(GitError::BranchNotFound(name.to_string()));
        }
        let current = self
            .inner
            .head()
            .ok()
            .and_then(|head| head.referent_name().map(|name| name.shorten().to_string()));
        if current.as_deref() == Some(name) {
            return Err(GitError::CurrentBranch(name.to_string()));
        }
        let full_name = format!("refs/heads/{name}");
        if self.inner.find_reference(full_name.as_str()).is_err() {
            return Err(GitError::BranchNotFound(name.to_string()));
        }

        use gix::refs::transaction::{Change, PreviousValue, RefEdit, RefLog};
        let ref_name = gix::refs::FullName::try_from(full_name)
            .map_err(|error| GitError::InvalidBranchName(error.to_string()))?;
        self.inner
            .edit_reference(RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::MustExist,
                    log: RefLog::AndReference,
                },
                name: ref_name,
                deref: false,
            })
            .map_err(gix_err)?;
        Ok(())
    }

    /// Lista a pilha de stash, da entrada mais recente para a mais antiga.
    ///
    /// Lê o reflog de `refs/stash` através do binário `git`; veja a
    /// documentação do módulo para a justificativa.
    pub fn stashes(&self) -> Result<Vec<StashInfo>, GitError> {
        let output = self.run_git(&["stash", "list", "--format=%H%x00%gs%x00%ci"])?;
        let mut stashes = Vec::new();
        for line in output.lines().filter(|line| !line.trim().is_empty()) {
            let mut fields = line.split('\0');
            let hash = fields.next().unwrap_or_default().trim();
            let message = fields.next().unwrap_or_default().trim();
            let date = fields.next().unwrap_or_default().trim();
            if hash.is_empty() {
                continue;
            }
            stashes.push(StashInfo {
                id: hash.chars().take(8).collect(),
                message: message.to_string(),
                time: date.chars().take(16).collect(),
            });
        }
        Ok(stashes)
    }

    /// Guarda as mudanças atuais (incluindo arquivos não rastreados) em um
    /// stash e limpa o worktree.
    pub fn stash_push(&self, message: &str) -> Result<(), GitError> {
        if self.status()?.is_clean() {
            return Err(GitError::NothingToStash);
        }
        let message = message.trim();
        let argument = if message.is_empty() {
            "--message=WIP (Arandu)".to_string()
        } else {
            format!("--message={message}")
        };
        self.run_git(&["stash", "push", "--include-untracked", &argument])?;
        Ok(())
    }

    /// Reaplica o stash identificado por `id` e o remove da pilha.
    ///
    /// `id` pode ser o id curto exibido por [`Repo::stashes`] ou um prefixo
    /// sem ambiguidade. Conflitos deixam o stash na pilha e retornam erro.
    pub fn stash_pop(&self, id: &str) -> Result<(), GitError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(GitError::StashNotFound(id.to_string()));
        }
        let stashes = self.stashes()?;
        let index = stashes
            .iter()
            .position(|stash| stash.id == id || stash.id.starts_with(id))
            .ok_or_else(|| GitError::StashNotFound(id.to_string()))?;
        let reference = format!("stash@{{{index}}}");
        self.run_git(&["stash", "pop", &reference])?;
        Ok(())
    }

    /// Executa o binário `git` no worktree e devolve o stdout.
    fn run_git(&self, args: &[&str]) -> Result<String, GitError> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .map_err(|error| GitError::Gix(format!("não foi possível executar git: {error}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitError::Gix(format!(
                "git {}: {}",
                args.join(" "),
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Autor e data formatada de um commit, com fallback vazio em falha.
    fn commit_meta(&self, id: gix::ObjectId) -> (String, String) {
        match self.inner.find_commit(id) {
            Ok(commit) => {
                let author = commit
                    .author()
                    .ok()
                    .map(|author| author.name.to_str_lossy().trim().to_string())
                    .unwrap_or_default();
                let time = commit
                    .time()
                    .ok()
                    .map(format_git_time)
                    .unwrap_or_else(|| "-".to_string());
                (author, time)
            }
            Err(_) => (String::new(), "-".to_string()),
        }
    }

    /// Abre o index; repositórios recém-inicializados ainda não têm `.git/index`.
    fn load_index(&self) -> Result<gix::index::File, GitError> {
        match self.inner.open_index() {
            Ok(index) => Ok(index),
            Err(_) => Ok(gix::index::File::from_state(
                gix::index::State::new(self.inner.object_hash()),
                self.inner.index_path(),
            )),
        }
    }

    fn write_index(&self, index: &mut gix::index::File) -> Result<(), GitError> {
        index
            .write(gix::index::write::Options {
                extensions: Default::default(),
                skip_hash: false,
            })
            .map_err(gix_err)
    }

    fn rela_path(&self, path: &Path) -> Result<BString, GitError> {
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| GitError::PathOutsideRepository(path.to_path_buf()))?;
        let normalized = relative.to_string_lossy().replace('\\', "/");
        if normalized.is_empty() || normalized == "." {
            return Err(GitError::InvalidPath(path.to_path_buf()));
        }
        Ok(BString::from(normalized))
    }

    fn identity(&self) -> (String, String) {
        let author = self.inner.author().and_then(Result::ok);
        let committer = self.inner.committer().and_then(Result::ok);
        let signature = committer.or(author);

        match signature {
            Some(signature) => {
                let name = signature.name.to_str_lossy().trim().to_string();
                let email = signature.email.to_str_lossy().trim().to_string();
                (
                    if name.is_empty() {
                        FALLBACK_NAME.to_string()
                    } else {
                        name
                    },
                    if email.is_empty() {
                        FALLBACK_EMAIL.to_string()
                    } else {
                        email
                    },
                )
            }
            None => (FALLBACK_NAME.to_string(), FALLBACK_EMAIL.to_string()),
        }
    }

    fn upstream_name(&self) -> Option<String> {
        let head = self.inner.head().ok()?;
        let name = head.referent_name()?;
        let upstream = self
            .inner
            .branch_remote_tracking_ref_name(name, gix::remote::Direction::Fetch)?
            .ok()?;
        Some(
            upstream
                .as_bstr()
                .to_str_lossy()
                .trim_start_matches("refs/remotes/")
                .to_string(),
        )
    }

    fn ahead_behind(&self) -> (usize, usize) {
        let head = match self.inner.head() {
            Ok(head) if !head.is_unborn() => head,
            _ => return (0, 0),
        };
        let head_id = match head.into_peeled_id() {
            Ok(id) => id.detach(),
            Err(_) => return (0, 0),
        };
        let upstream_id = match self.upstream_id() {
            Some(id) => id,
            None => return (0, 0),
        };
        if head_id == upstream_id {
            return (0, 0);
        }

        let head_ancestors = self.ancestors(head_id);
        let upstream_ancestors = self.ancestors(upstream_id);
        let ahead = head_ancestors.difference(&upstream_ancestors).count();
        let behind = upstream_ancestors.difference(&head_ancestors).count();
        (ahead, behind)
    }

    fn upstream_id(&self) -> Option<gix::ObjectId> {
        let head = self.inner.head().ok()?;
        let name = head.referent_name()?;
        let upstream = self
            .inner
            .branch_remote_tracking_ref_name(name, gix::remote::Direction::Fetch)?
            .ok()?;
        let mut reference = self.inner.find_reference(upstream.as_bstr()).ok()?;
        Some(reference.peel_to_id().ok()?.detach())
    }

    fn ancestors(&self, start: gix::ObjectId) -> HashSet<gix::ObjectId> {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([start]);
        while let Some(id) = queue.pop_front() {
            if seen.len() >= MAX_GRAPH_WALK || !seen.insert(id) {
                continue;
            }
            let Ok(commit) = self.inner.find_commit(id) else {
                continue;
            };
            for parent in commit.parent_ids() {
                let parent = parent.detach();
                if !seen.contains(&parent) {
                    queue.push_back(parent);
                }
            }
        }
        seen
    }

    /// Mapa `path -> (id, mode)` com todas as folhas da árvore de `HEAD`.
    fn head_entries(&self) -> Result<HashMap<Vec<u8>, (gix::ObjectId, IndexMode)>, GitError> {
        let mut entries = HashMap::new();
        let mut head = self.inner.head().map_err(gix_err)?;
        if head.is_unborn() {
            return Ok(entries);
        }
        let tree_id = head
            .peel_to_commit()
            .map_err(gix_err)?
            .tree_id()
            .map_err(gix_err)?
            .detach();
        self.collect_tree(tree_id, &mut Vec::new(), &mut entries)?;
        Ok(entries)
    }

    fn collect_tree(
        &self,
        tree_id: gix::ObjectId,
        prefix: &mut Vec<u8>,
        out: &mut HashMap<Vec<u8>, (gix::ObjectId, IndexMode)>,
    ) -> Result<(), GitError> {
        let tree = self.inner.find_tree(tree_id).map_err(gix_err)?;
        for entry in tree.iter() {
            let entry = entry.map_err(gix_err)?;
            let previous_len = prefix.len();
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(entry.inner.filename);
            if entry.inner.mode.is_tree() {
                self.collect_tree(entry.inner.oid.to_owned(), prefix, out)?;
            } else {
                let mode = index_mode(entry.inner.mode);
                out.insert(prefix.clone(), (entry.inner.oid.to_owned(), mode));
            }
            prefix.truncate(previous_len);
        }
        Ok(())
    }

    /// Converte o index atual em uma árvore Git, gravando-a no object database.
    fn write_tree_from_index(&self, index: &gix::index::File) -> Result<gix::ObjectId, GitError> {
        let entries: Vec<(BString, IndexMode, gix::ObjectId)> = index
            .entries()
            .iter()
            .filter(|entry| entry.stage() == gix::index::entry::Stage::Unconflicted)
            .map(|entry| {
                (
                    entry.path(index).to_owned(),
                    entry.mode,
                    entry.id.to_owned(),
                )
            })
            .collect();
        self.write_tree_recursive(&entries)
    }

    fn write_tree_recursive(
        &self,
        entries: &[(BString, IndexMode, gix::ObjectId)],
    ) -> Result<gix::ObjectId, GitError> {
        let mut tree = gix::objs::Tree::empty();
        let mut index = 0;

        while index < entries.len() {
            let (path, mode, id) = &entries[index];
            if let Some(slash) = path.iter().position(|b| *b == b'/') {
                let directory = &path[..slash];
                let mut end = index + 1;
                while end < entries.len() {
                    let candidate = &entries[end].0;
                    if candidate.starts_with(directory)
                        && candidate.get(directory.len()) == Some(&b'/')
                    {
                        end += 1;
                    } else {
                        break;
                    }
                }
                let children: Vec<(BString, IndexMode, gix::ObjectId)> = entries[index..end]
                    .iter()
                    .map(|(child, mode, id)| (BString::from(&child[slash + 1..]), *mode, *id))
                    .collect();
                let subtree = self.write_tree_recursive(&children)?;
                tree.entries.push(gix::objs::tree::Entry {
                    mode: EntryMode::from(EntryKind::Tree),
                    filename: BString::from(directory),
                    oid: subtree,
                });
                index = end;
            } else {
                tree.entries.push(gix::objs::tree::Entry {
                    mode: mode
                        .to_tree_entry_mode()
                        .ok_or_else(|| GitError::Gix("modo de entrada inválido".into()))?,
                    filename: path.clone(),
                    oid: *id,
                });
                index += 1;
            }
        }

        tree.entries.sort();
        Ok(self.inner.write_object(tree).map_err(gix_err)?.detach())
    }
}

fn tree_index_change(change: &gix::diff::index::Change) -> Vec<FileEntry> {
    use gix::diff::index::Change;
    match change {
        Change::Addition { location, .. } => vec![FileEntry {
            path: to_path(location.as_ref()),
            status: FileStatus::Added,
        }],
        Change::Deletion { location, .. } => vec![FileEntry {
            path: to_path(location.as_ref()),
            status: FileStatus::Deleted,
        }],
        Change::Modification { location, .. } => vec![FileEntry {
            path: to_path(location.as_ref()),
            status: FileStatus::Modified,
        }],
        Change::Rewrite {
            source_location,
            location,
            copy,
            ..
        } => {
            if *copy {
                vec![FileEntry {
                    path: to_path(location.as_ref()),
                    status: FileStatus::Added,
                }]
            } else {
                vec![
                    FileEntry {
                        path: to_path(source_location.as_ref()),
                        status: FileStatus::Deleted,
                    },
                    FileEntry {
                        path: to_path(location.as_ref()),
                        status: FileStatus::Renamed,
                    },
                ]
            }
        }
    }
}

fn entry_status(
    status: &gix::status::plumbing::index_as_worktree::EntryStatus<(), gix::submodule::Status>,
) -> Option<FileStatus> {
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
    match status {
        EntryStatus::Change(Change::Removed) => Some(FileStatus::Deleted),
        EntryStatus::Change(
            Change::Modification { .. } | Change::Type { .. } | Change::SubmoduleModification(_),
        ) => Some(FileStatus::Modified),
        EntryStatus::Conflict { .. } => Some(FileStatus::Modified),
        EntryStatus::IntentToAdd => Some(FileStatus::Added),
        EntryStatus::NeedsUpdate(_) => None,
    }
}

fn index_mode(mode: EntryMode) -> IndexMode {
    match EntryKind::from(mode) {
        EntryKind::Tree => IndexMode::DIR,
        EntryKind::Blob => IndexMode::FILE,
        EntryKind::BlobExecutable => IndexMode::FILE_EXECUTABLE,
        EntryKind::Link => IndexMode::SYMLINK,
        EntryKind::Commit => IndexMode::COMMIT,
    }
}

fn to_path(path: &BStr) -> PathBuf {
    PathBuf::from(path.to_str_lossy().into_owned())
}

/// Valida um nome de branch segundo as regras básicas do `git check-ref-format`.
///
/// A validação é deliberadamente conservadora: cobre os casos que produzem
/// refs inválidas ou confundem a interface, sem tentar reproduzir todas as
/// regras de `git check-ref-format`.
fn validate_branch_name(name: &str) -> Result<&str, GitError> {
    let name = name.trim();
    let invalid = name.is_empty()
        || name == "HEAD"
        || name.starts_with('-')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.chars().any(|character| {
            matches!(
                character,
                ' ' | '\t' | '\n' | '~' | '^' | ':' | '?' | '*' | '[' | '\\' | '\x7f'
            )
        });
    if invalid {
        return Err(GitError::InvalidBranchName(name.to_string()));
    }
    Ok(name)
}

fn sort_dedup(entries: &mut Vec<FileEntry>) {
    entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.status.cmp(&b.status)));
    entries.dedup();
}

/// Id curto de 8 caracteres hexadecimais.
fn short_id(id: gix::ObjectId) -> String {
    id.to_hex_with_len(8).to_string()
}

/// Formata um instante Git como `YYYY-MM-DD HH:MM` no fuso do commit.
fn format_git_time(time: gix::date::Time) -> String {
    let seconds = time.seconds + i64::from(time.offset);
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3_600,
        (rem % 3_600) / 60
    )
}

/// Converte dias desde 1970-01-01 em (ano, mês, dia) — algoritmo de Hinnant.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Renderiza o grafo ASCII do log, uma string por commit.
///
/// O resultado é uma aproximação determinística de
/// `git log --graph --oneline`: colunas representam lanes ativas, `*` marca o
/// commit da linha e merges acrescentam o marcador `|\`. A função é pura e
/// não depende do repositório, o que facilita os testes.
pub fn render_graph(commits: &[CommitInfo]) -> Vec<String> {
    let mut lanes: Vec<&str> = Vec::new();
    let mut graph = Vec::with_capacity(commits.len());

    for commit in commits {
        let lane = match lanes.iter().position(|id| *id == commit.id) {
            Some(index) => index,
            None => {
                lanes.push(commit.id.as_str());
                lanes.len() - 1
            }
        };

        let mut row = String::new();
        for column in 0..lanes.len() {
            row.push(if column == lane { '*' } else { '|' });
            row.push(' ');
        }

        lanes.remove(lane);
        for (offset, parent) in commit.parents.iter().enumerate() {
            if lanes.contains(&parent.as_str()) {
                continue;
            }
            let position = (lane + offset).min(lanes.len());
            lanes.insert(position, parent.as_str());
        }

        if commit.parents.len() >= 2 {
            row.push_str("|\\ ");
        }
        graph.push(row);
    }

    graph
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct TestRepo {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    impl TestRepo {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().to_path_buf();
            git(&path, &["init", "-b", "main"]);
            git(&path, &["config", "user.name", "Teste"]);
            git(&path, &["config", "user.email", "teste@example.com"]);
            Self { _dir: dir, path }
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            std::fs::write(path, contents).expect("write");
        }

        fn git(&self, args: &[&str]) -> String {
            git(&self.path, args)
        }
    }

    fn git(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(path)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("executar git");
        assert!(
            output.status.success(),
            "git {:?} falhou: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn discover_encontra_repo_em_subdiretorio() {
        let repo = TestRepo::new();
        let nested = repo.path.join("a/b/c");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let found = Repo::discover(&nested).expect("repositório descoberto");
        assert_eq!(
            std::fs::canonicalize(found.root()).unwrap(),
            std::fs::canonicalize(&repo.path).unwrap()
        );
    }

    #[test]
    fn status_de_repo_novo_e_limpo() {
        let repo = TestRepo::new();
        let status = Repo::discover(&repo.path).unwrap().status().unwrap();
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(status.is_clean());
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn untracked_aparece_no_status() {
        let repo = TestRepo::new();
        repo.write("novo.txt", "conteúdo");
        let status = Repo::discover(&repo.path).unwrap().status().unwrap();
        assert_eq!(status.untracked.len(), 1);
        assert_eq!(status.untracked[0].path, PathBuf::from("novo.txt"));
        assert_eq!(status.untracked[0].status, FileStatus::Added);
    }

    #[test]
    fn stage_move_arquivo_para_staged() {
        let repo = TestRepo::new();
        repo.write("novo.txt", "conteúdo");
        let repo = Repo::discover(&repo.path).unwrap();
        repo.stage(&[repo.root().join("novo.txt")]).unwrap();

        let status = repo.status().unwrap();
        assert!(status.untracked.is_empty());
        assert_eq!(status.staged.len(), 1);
        assert_eq!(status.staged[0].path, PathBuf::from("novo.txt"));
        assert_eq!(status.staged[0].status, FileStatus::Added);
    }

    #[test]
    fn unstage_volta_arquivo_para_untracked() {
        let repo = TestRepo::new();
        repo.write("novo.txt", "conteúdo");
        let repo = Repo::discover(&repo.path).unwrap();
        let path = repo.root().join("novo.txt");
        repo.stage(std::slice::from_ref(&path)).unwrap();
        repo.unstage(&[path]).unwrap();

        let status = repo.status().unwrap();
        assert!(status.staged.is_empty());
        assert_eq!(status.untracked.len(), 1);
    }

    #[test]
    fn commit_cria_commit_com_mensagem_correta() {
        let repo = TestRepo::new();
        repo.write("novo.txt", "conteúdo");
        let git_repo = Repo::discover(&repo.path).unwrap();
        git_repo.stage(&[git_repo.root().join("novo.txt")]).unwrap();
        let oid = git_repo.commit("feat: primeiro commit").unwrap();

        let message = repo.git(&["log", "-1", "--format=%s", &oid.to_string()]);
        assert_eq!(message, "feat: primeiro commit");
    }

    #[test]
    fn status_pos_commit_e_limpo() {
        let repo = TestRepo::new();
        repo.write("novo.txt", "conteúdo");
        let repo = Repo::discover(&repo.path).unwrap();
        repo.stage(&[repo.root().join("novo.txt")]).unwrap();
        repo.commit("chore: commit inicial").unwrap();

        let status = repo.status().unwrap();
        assert!(status.is_clean());
        assert_eq!(status.staged.len(), 0);
        assert_eq!(status.modified.len(), 0);
        assert_eq!(status.untracked.len(), 0);
    }

    #[test]
    fn ahead_e_behind_contra_upstream() {
        let repo = TestRepo::new();
        repo.write("a.txt", "1");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "inicial"]);

        let clone_dir = tempfile::tempdir().expect("tempdir do clone");
        let clone = clone_dir.path().join("clone");
        let clone_str = clone.to_str().expect("utf-8").to_owned();
        repo.git(&["clone", repo.path.to_str().expect("utf-8"), &clone_str]);
        git(&clone, &["config", "user.name", "Clone"]);
        git(&clone, &["config", "user.email", "clone@example.com"]);
        std::fs::write(clone.join("b.txt"), "2").unwrap();
        git(&clone, &["add", "."]);
        git(&clone, &["commit", "-m", "segundo"]);

        repo.git(&["remote", "add", "origin", &clone_str]);
        repo.git(&["fetch", "origin"]);
        repo.git(&["branch", "--set-upstream-to=origin/main", "main"]);

        let status = Repo::discover(&repo.path).unwrap().status().unwrap();
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.behind, 1);
        assert_eq!(status.ahead, 0);

        repo.write("c.txt", "3");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "terceiro"]);
        let status = Repo::discover(&repo.path).unwrap().status().unwrap();
        assert_eq!(status.ahead, 1);
        assert_eq!(status.behind, 1);
    }

    #[test]
    fn branch_name_correto() {
        let repo = TestRepo::new();
        repo.git(&["checkout", "-b", "feat/painel-git"]);
        let status = Repo::discover(&repo.path).unwrap().status().unwrap();
        assert_eq!(status.branch.as_deref(), Some("feat/painel-git"));
    }

    #[test]
    fn modified_detecta_arquivo_rastreado_alterado() {
        let repo = TestRepo::new();
        repo.write("tracked.txt", "versão 1");
        let repo = Repo::discover(&repo.path).unwrap();
        repo.stage(&[repo.root().join("tracked.txt")]).unwrap();
        repo.commit("chore: inicial").unwrap();

        std::fs::write(repo.root().join("tracked.txt"), "versão 2").unwrap();
        let status = repo.status().unwrap();
        assert_eq!(status.modified.len(), 1);
        assert_eq!(status.modified[0].path, PathBuf::from("tracked.txt"));
        assert_eq!(status.modified[0].status, FileStatus::Modified);
    }

    #[test]
    fn log_linear_lista_commits_do_mais_novo_ao_mais_antigo() {
        let repo = TestRepo::new();
        repo.write("a.txt", "1");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "primeiro"]);
        repo.write("b.txt", "2");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "segundo"]);
        repo.write("c.txt", "3");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "terceiro"]);

        let log = Repo::discover(&repo.path).unwrap().log(10).unwrap();
        let messages: Vec<&str> = log.iter().map(|commit| commit.message.as_str()).collect();
        assert_eq!(messages, vec!["terceiro", "segundo", "primeiro"]);
        assert!(log.iter().all(|commit| commit.id.len() == 8));
        assert_eq!(log[0].parents.len(), 1);
        assert!(log.last().unwrap().parents.is_empty());
        assert_eq!(log[0].author, "Teste");

        let graph = render_graph(&log);
        assert_eq!(graph, vec!["* ", "* ", "* "]);
    }

    #[test]
    fn log_com_merge_marca_grafo_com_linha_de_bifurcacao() {
        let repo = TestRepo::new();
        repo.write("base.txt", "base");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "base"]);

        repo.git(&["checkout", "-b", "feature"]);
        repo.write("feature.txt", "f");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "feature"]);

        repo.git(&["checkout", "main"]);
        repo.write("main.txt", "m");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "main"]);
        repo.git(&["merge", "--no-ff", "feature", "-m", "merge feature"]);

        let log = Repo::discover(&repo.path).unwrap().log(20).unwrap();
        assert_eq!(log[0].message, "merge feature");
        assert_eq!(log[0].parents.len(), 2, "merge deve ter dois pais");

        let graph = render_graph(&log);
        assert_eq!(graph.len(), log.len());
        assert!(
            graph.iter().any(|line| line.contains('|')),
            "grafo de merge deveria conter '|': {graph:?}"
        );
    }

    #[test]
    fn blame_atribui_linhas_aos_commits_certos() {
        let repo = TestRepo::new();
        repo.write("linhas.txt", "um\ndois\ntres\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "inicial"]);
        let first = repo.git(&["rev-parse", "HEAD"]);

        repo.write("linhas.txt", "UM\ndois\ntres\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "altera primeira"]);
        let second = repo.git(&["rev-parse", "HEAD"]);

        let repo = Repo::discover(&repo.path).unwrap();
        let blame = repo
            .blame_file(&repo.root().join("linhas.txt"), 0)
            .expect("blame");

        assert_eq!(blame.len(), 3);
        assert_eq!(blame[0].commit, second[..8]);
        assert_eq!(blame[1].commit, first[..8]);
        assert_eq!(blame[2].commit, first[..8]);
        let contents: Vec<&str> = blame.iter().map(|line| line.content.as_str()).collect();
        assert_eq!(contents, vec!["UM", "dois", "tres"]);
        assert_eq!(blame[0].author, "Teste");
    }

    #[test]
    fn blame_respeita_cap_de_linhas() {
        let repo = TestRepo::new();
        let contents = (1..=10)
            .map(|line| format!("linha {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        repo.write("longo.txt", &contents);
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "inicial"]);

        let repo = Repo::discover(&repo.path).unwrap();
        let blame = repo.blame_file(&repo.root().join("longo.txt"), 4).unwrap();
        assert_eq!(blame.len(), 4);
        assert_eq!(blame[0].content, "linha 1");
        assert_eq!(blame[3].content, "linha 4");
    }

    fn repo_com_commit(repo: &TestRepo) -> Repo {
        repo.write("arquivo.txt", "inicial");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "inicial"]);
        Repo::discover(&repo.path).expect("repositório")
    }

    #[test]
    fn branches_lista_atual_marcada() {
        let repo = TestRepo::new();
        let repo = repo_com_commit(&repo);
        repo.create_branch("feature").unwrap();

        let branches = repo.branches().unwrap();
        assert_eq!(branches.len(), 2);
        let main = branches.iter().find(|b| b.name == "main").unwrap();
        let feature = branches.iter().find(|b| b.name == "feature").unwrap();
        assert!(main.current, "main deveria estar marcada como atual");
        assert!(!feature.current);
        assert_eq!(main.commit.len(), 8);
        assert_eq!(feature.commit, main.commit);
        assert_eq!(branches[0].name, "main", "a atual vem primeiro");
    }

    #[test]
    fn create_branch_aponta_para_head_e_rejeita_nome_invalido() {
        let repo = TestRepo::new();
        let repo = repo_com_commit(&repo);
        repo.create_branch("nova").unwrap();

        let head = repo
            .inner
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .detach();
        let created = repo
            .inner
            .find_reference("refs/heads/nova")
            .unwrap()
            .try_id()
            .unwrap()
            .detach();
        assert_eq!(created, head);
        assert!(matches!(
            repo.create_branch("nome com espaço"),
            Err(GitError::InvalidBranchName(_))
        ));
        assert!(matches!(
            repo.create_branch("  "),
            Err(GitError::InvalidBranchName(_))
        ));
    }

    #[test]
    fn checkout_troca_branch_e_atualiza_worktree() {
        let fixture = TestRepo::new();
        let repo = repo_com_commit(&fixture);
        repo.create_branch("feature").unwrap();
        repo.checkout("feature").unwrap();

        let status = repo.status().unwrap();
        assert_eq!(status.branch.as_deref(), Some("feature"));

        fixture.write("arquivo.txt", "da feature");
        fixture.write("exclusivo.txt", "só na feature");
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "altera na feature"]);
        repo.checkout("main").unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.root().join("arquivo.txt")).unwrap(),
            "inicial"
        );
        assert!(
            !repo.root().join("exclusivo.txt").exists(),
            "checkout deveria remover arquivos ausentes na branch de destino"
        );
        repo.checkout("feature").unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.root().join("arquivo.txt")).unwrap(),
            "da feature"
        );
        assert_eq!(
            std::fs::read_to_string(repo.root().join("exclusivo.txt")).unwrap(),
            "só na feature"
        );
    }

    #[test]
    fn checkout_bloqueia_worktree_sujo() {
        let fixture = TestRepo::new();
        let repo = repo_com_commit(&fixture);
        repo.create_branch("feature").unwrap();
        fixture.write("arquivo.txt", "modificado");
        assert!(matches!(
            repo.checkout("feature"),
            Err(GitError::DirtyWorktree)
        ));
        assert_eq!(repo.status().unwrap().branch.as_deref(), Some("main"));
    }

    #[test]
    fn delete_branch_remove_ref_e_rejeita_atual() {
        let repo = TestRepo::new();
        let repo = repo_com_commit(&repo);
        repo.create_branch("descartavel").unwrap();
        repo.delete_branch("descartavel").unwrap();
        assert!(
            repo.branches()
                .unwrap()
                .iter()
                .all(|branch| branch.name != "descartavel")
        );
        assert!(matches!(
            repo.delete_branch("main"),
            Err(GitError::CurrentBranch(_))
        ));
        assert!(matches!(
            repo.delete_branch("nao-existe"),
            Err(GitError::BranchNotFound(_))
        ));
    }

    #[test]
    fn stash_guarda_lista_e_reaplica_mudancas() {
        let fixture = TestRepo::new();
        let repo = repo_com_commit(&fixture);
        fixture.write("arquivo.txt", "alterado");
        fixture.write("novo.txt", "não rastreado");
        repo.stash_push("trabalho em andamento").unwrap();

        assert!(repo.status().unwrap().is_clean());
        let stashes = repo.stashes().unwrap();
        assert_eq!(stashes.len(), 1);
        assert!(stashes[0].message.contains("trabalho em andamento"));
        assert_eq!(stashes[0].id.len(), 8);
        assert_eq!(stashes[0].time.len(), 16);

        let id = stashes[0].id.clone();
        repo.stash_pop(&id).unwrap();
        assert!(repo.stashes().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(repo.root().join("arquivo.txt")).unwrap(),
            "alterado"
        );
        assert_eq!(
            std::fs::read_to_string(repo.root().join("novo.txt")).unwrap(),
            "não rastreado"
        );
    }

    #[test]
    fn stash_push_sem_mudancas_retorna_erro() {
        let repo = TestRepo::new();
        let repo = repo_com_commit(&repo);
        assert!(matches!(
            repo.stash_push("vazio"),
            Err(GitError::NothingToStash)
        ));
        assert!(matches!(
            repo.stash_pop("deadbeef"),
            Err(GitError::StashNotFound(_))
        ));
    }

    #[test]
    fn render_graph_e_deterministico_para_merge() {
        let commits = vec![
            CommitInfo {
                id: "aaaaaaaa".into(),
                message: "merge".into(),
                author: "A".into(),
                time: "2026-01-01 00:00".into(),
                parents: vec!["bbbbbbbb".into(), "cccccccc".into()],
            },
            CommitInfo {
                id: "bbbbbbbb".into(),
                message: "main".into(),
                author: "A".into(),
                time: "2026-01-01 00:00".into(),
                parents: vec!["dddddddd".into()],
            },
            CommitInfo {
                id: "cccccccc".into(),
                message: "feature".into(),
                author: "A".into(),
                time: "2026-01-01 00:00".into(),
                parents: vec!["dddddddd".into()],
            },
        ];
        let graph = render_graph(&commits);
        assert_eq!(graph, vec!["* |\\ ", "* | ", "| * "]);
    }
}
