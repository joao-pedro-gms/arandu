//! Engine de operações de arquivo, independente de interface.
//!
//! Toda operação reporta progresso arquivo a arquivo (nunca no meio de um
//! arquivo) e consulta um [`AtomicBool`] de cancelamento entre arquivos. O
//! cancelamento é cooperativo: um arquivo já iniciado é concluído antes de a
//! operação abortar.
//!
//! # Lixeira
//!
//! Exclusões vão para a lixeira do sistema operacional via crate `trash` e
//! **não são desfazíveis**: a crate não oferece restauração. [`undo`] devolve
//! `None` para operações de exclusão.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

/// Tipo de operação de arquivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    /// Copia origem para o diretório de destino.
    Copy,
    /// Move origem para o diretório de destino.
    Move,
    /// Envia origem para a lixeira do sistema.
    Delete,
    /// Renomeia origem dentro do próprio diretório.
    Rename,
}

impl OpKind {
    /// Se a operação pode ser desfeita por [`undo`].
    pub fn undoable(self) -> bool {
        !matches!(self, Self::Delete)
    }
}

/// Parâmetros de uma operação.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpParams {
    /// Caminho de origem.
    pub src: PathBuf,
    /// Diretório de destino (cópia/movimento) ou novo nome (renomeação).
    pub dest: Option<PathBuf>,
}

/// Operação enfileirável.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    /// Tipo da operação.
    pub kind: OpKind,
    /// Parâmetros da operação.
    pub params: OpParams,
}

impl Op {
    /// Cria uma operação.
    pub fn new(kind: OpKind, src: impl Into<PathBuf>, dest: Option<PathBuf>) -> Self {
        Self {
            kind,
            params: OpParams {
                src: src.into(),
                dest,
            },
        }
    }
}

/// Progresso de uma operação, reportado após cada arquivo concluído.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// Arquivo recém-concluído.
    pub current: PathBuf,
    /// Bytes já processados.
    pub bytes_done: u64,
    /// Bytes previstos no início da operação.
    pub bytes_total: u64,
    /// Arquivos já processados.
    pub files_done: u64,
    /// Arquivos previstos no início da operação.
    pub files_total: u64,
}

impl Progress {
    /// Fração concluída em `0.0..=1.0`, tolerante a estimativas desatualizadas.
    ///
    /// Usa bytes quando disponíveis (progresso mais suave em árvores com
    /// arquivos de tamanhos diferentes) e cai para contagem de arquivos quando
    /// o total em bytes é zero.
    pub fn fraction(&self) -> f32 {
        if self.bytes_total > 0 {
            (self.bytes_done as f32 / self.bytes_total as f32).clamp(0.0, 1.0)
        } else if self.files_total > 0 {
            (self.files_done as f32 / self.files_total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Operação concluída com sucesso, pronta para [`undo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedOp {
    /// Tipo executado.
    pub kind: OpKind,
    /// Origem original.
    pub source: PathBuf,
    /// Destino efetivo (após resolução de conflitos de nome).
    pub destination: PathBuf,
}

/// Erros das operações de arquivo.
#[derive(Debug, Error)]
pub enum OpsError {
    /// Operação cancelada entre arquivos.
    #[error("operação cancelada")]
    Cancelled,

    /// Origem inexistente.
    #[error("origem não encontrada: {path}")]
    NotFound {
        /// Caminho informado.
        path: PathBuf,
    },

    /// Sem permissão para acessar o caminho.
    #[error("permissão negada ao acessar: {path}")]
    PermissionDenied {
        /// Caminho informado.
        path: PathBuf,
    },

    /// Operação sem destino obrigatório.
    #[error("operação exige um destino")]
    MissingDestination,

    /// Nome de renomeação inválido.
    #[error("nome inválido: {name}")]
    InvalidName {
        /// Nome informado.
        name: String,
    },

    /// Falha ao enviar para a lixeira.
    #[error("falha ao mover {path} para a lixeira: {message}")]
    Trash {
        /// Caminho informado.
        path: PathBuf,
        /// Mensagem devolvida pela crate `trash`.
        message: String,
    },

    /// Outro erro de I/O.
    #[error("erro de I/O em {path}")]
    Io {
        /// Caminho relacionado ao erro.
        path: PathBuf,
        /// Erro original do sistema.
        #[source]
        source: io::Error,
    },
}

impl OpsError {
    fn from_io(path: &Path, source: io::Error) -> Self {
        match source.kind() {
            io::ErrorKind::NotFound => Self::NotFound {
                path: path.to_path_buf(),
            },
            io::ErrorKind::PermissionDenied => Self::PermissionDenied {
                path: path.to_path_buf(),
            },
            _ => Self::Io {
                path: path.to_path_buf(),
                source,
            },
        }
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), OpsError> {
    if cancel.load(Ordering::SeqCst) {
        Err(OpsError::Cancelled)
    } else {
        Ok(())
    }
}

/// Calcula um destino livre em `dir`, acrescentando ` (2)`, ` (3)`, … quando
/// `name` já existe. Nunca sobrescreve.
///
/// A extensão é preservada: `foto.png` → `foto (2).png`.
pub fn unique_destination(dir: &Path, name: impl AsRef<OsStr>) -> PathBuf {
    let name = name.as_ref();
    let candidate = dir.join(name);
    if fs::symlink_metadata(&candidate).is_err() {
        return candidate;
    }

    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(OsStr::to_os_string)
        .unwrap_or_else(|| name.to_os_string());
    let extension = path.extension().map(OsStr::to_os_string);

    let mut counter = 2_u64;
    loop {
        let mut suffixed = stem.clone();
        suffixed.push(format!(" ({counter})"));
        if let Some(extension) = &extension {
            suffixed.push(".");
            suffixed.push(extension);
        }
        let candidate = dir.join(suffixed);
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        counter += 1;
    }
}

/// Copia `src` (arquivo ou diretório) para `dest_dir`, resolvendo conflitos de
/// nome com [`unique_destination`].
///
/// Devolve o caminho efetivo criado. O progresso é reportado após cada arquivo.
pub fn copy_recursive(
    src: impl AsRef<Path>,
    dest_dir: impl AsRef<Path>,
    on_progress: &dyn Fn(Progress),
) -> Result<PathBuf, OpsError> {
    copy_recursive_with_cancel(
        src.as_ref(),
        dest_dir.as_ref(),
        on_progress,
        &AtomicBool::new(false),
    )
}

/// Variante de [`copy_recursive`] com cancelamento cooperativo.
pub fn copy_recursive_with_cancel(
    src: &Path,
    dest_dir: &Path,
    on_progress: &dyn Fn(Progress),
    cancel: &AtomicBool,
) -> Result<PathBuf, OpsError> {
    ensure_exists(src)?;
    let name = src.file_name().ok_or_else(|| OpsError::InvalidName {
        name: src.display().to_string(),
    })?;
    let destination = unique_destination(dest_dir, name);
    copy_to(src, &destination, on_progress, cancel)?;
    Ok(destination)
}

/// Move `src` (arquivo ou diretório) para `dest_dir`, resolvendo conflitos de
/// nome. Usa `rename` no mesmo volume e cópia + remoção da origem em volumes
/// distintos (`EXDEV`).
pub fn move_entry(
    src: impl AsRef<Path>,
    dest_dir: impl AsRef<Path>,
    on_progress: &dyn Fn(Progress),
) -> Result<PathBuf, OpsError> {
    move_entry_with_cancel(
        src.as_ref(),
        dest_dir.as_ref(),
        on_progress,
        &AtomicBool::new(false),
    )
}

/// Variante de [`move_entry`] com cancelamento cooperativo.
pub fn move_entry_with_cancel(
    src: &Path,
    dest_dir: &Path,
    on_progress: &dyn Fn(Progress),
    cancel: &AtomicBool,
) -> Result<PathBuf, OpsError> {
    ensure_exists(src)?;
    let name = src.file_name().ok_or_else(|| OpsError::InvalidName {
        name: src.display().to_string(),
    })?;
    let destination = unique_destination(dest_dir, name);

    match fs::rename(src, &destination) {
        Ok(()) => Ok(destination),
        // Volumes distintos: sem `rename` atômico, copia e apaga a origem.
        Err(err) if err.kind() == io::ErrorKind::CrossesDevices => {
            copy_to(src, &destination, on_progress, cancel)?;
            remove_source(src)?;
            Ok(destination)
        }
        Err(err) => Err(OpsError::from_io(src, err)),
    }
}

/// Renomeia `src` para `new_name` dentro do mesmo diretório.
///
/// Conflitos de nome são resolvidos com [`unique_destination`] (nunca
/// sobrescreve) e o caminho efetivo é devolvido.
pub fn rename_entry(src: impl AsRef<Path>, new_name: &str) -> Result<PathBuf, OpsError> {
    let src = src.as_ref();
    ensure_exists(src)?;
    let new_name = new_name.trim();
    let mut components = Path::new(new_name).components();
    let valid = !new_name.is_empty()
        && matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none();
    if !valid {
        return Err(OpsError::InvalidName {
            name: new_name.to_owned(),
        });
    }

    if src.file_name() == Some(OsStr::new(new_name)) {
        return Ok(src.to_path_buf());
    }

    let parent = src.parent().ok_or_else(|| OpsError::InvalidName {
        name: src.display().to_string(),
    })?;
    let destination = unique_destination(parent, new_name);
    fs::rename(src, &destination).map_err(|err| OpsError::from_io(src, err))?;
    Ok(destination)
}

/// Envia `path` para a lixeira do sistema operacional (recursivo para
/// diretórios). A crate `trash` não restaura itens, então a exclusão não é
/// desfazível.
pub fn delete_to_trash(path: impl AsRef<Path>) -> Result<(), OpsError> {
    let path = path.as_ref();
    trash::delete(path).map_err(|err| OpsError::Trash {
        path: path.to_path_buf(),
        message: err.to_string(),
    })
}

/// Constrói a operação inversa de `completed`, ou `None` quando não há undo
/// (exclusão).
pub fn undo(completed: &CompletedOp) -> Option<Op> {
    match completed.kind {
        // O destino da cópia é enviado para a lixeira.
        OpKind::Copy => Some(Op::new(OpKind::Delete, completed.destination.clone(), None)),
        OpKind::Move => {
            let parent = completed.source.parent()?.to_path_buf();
            Some(Op::new(
                OpKind::Move,
                completed.destination.clone(),
                Some(parent),
            ))
        }
        OpKind::Rename => {
            let name = completed.source.file_name()?.to_os_string();
            Some(Op::new(
                OpKind::Rename,
                completed.destination.clone(),
                Some(PathBuf::from(name)),
            ))
        }
        OpKind::Delete => None,
    }
}

/// Executa uma operação, reportando progresso e consultando `cancel` entre
/// arquivos.
pub fn execute(
    op: &Op,
    on_progress: &dyn Fn(Progress),
    cancel: &AtomicBool,
) -> Result<CompletedOp, OpsError> {
    check_cancel(cancel)?;
    let src = &op.params.src;
    ensure_exists(src)?;

    match op.kind {
        OpKind::Copy => {
            let dest_dir = op
                .params
                .dest
                .as_deref()
                .ok_or(OpsError::MissingDestination)?;
            let destination = copy_recursive_with_cancel(src, dest_dir, on_progress, cancel)?;
            Ok(CompletedOp {
                kind: OpKind::Copy,
                source: src.clone(),
                destination,
            })
        }
        OpKind::Move => {
            let dest_dir = op
                .params
                .dest
                .as_deref()
                .ok_or(OpsError::MissingDestination)?;
            let destination = move_entry_with_cancel(src, dest_dir, on_progress, cancel)?;
            Ok(CompletedOp {
                kind: OpKind::Move,
                source: src.clone(),
                destination,
            })
        }
        OpKind::Delete => {
            check_cancel(cancel)?;
            delete_to_trash(src)?;
            Ok(CompletedOp {
                kind: OpKind::Delete,
                source: src.clone(),
                destination: src.clone(),
            })
        }
        OpKind::Rename => {
            let raw = op
                .params
                .dest
                .as_deref()
                .ok_or(OpsError::MissingDestination)?;
            let name = raw.to_str().ok_or_else(|| OpsError::InvalidName {
                name: raw.display().to_string(),
            })?;
            let destination = rename_entry(src, name)?;
            Ok(CompletedOp {
                kind: OpKind::Rename,
                source: src.clone(),
                destination,
            })
        }
    }
}

fn ensure_exists(path: &Path) -> Result<(), OpsError> {
    fs::symlink_metadata(path)
        .map(|_| ())
        .map_err(|err| OpsError::from_io(path, err))
}

fn copy_to(
    src: &Path,
    destination: &Path,
    on_progress: &dyn Fn(Progress),
    cancel: &AtomicBool,
) -> Result<(), OpsError> {
    let metadata = fs::symlink_metadata(src).map_err(|err| OpsError::from_io(src, err))?;

    if !metadata.is_dir() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|err| OpsError::from_io(parent, err))?;
        }
        fs::copy(src, destination).map_err(|err| OpsError::from_io(src, err))?;
        on_progress(Progress {
            current: src.to_path_buf(),
            bytes_done: metadata.len(),
            bytes_total: metadata.len(),
            files_done: 1,
            files_total: 1,
        });
        return Ok(());
    }

    // Walk prévio para estimar o total; se o diretório crescer durante a
    // cópia, o total é ajustado para cima (progresso nunca passa de 100%).
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut bytes_total = 0_u64;
    walk(src, &mut dirs, &mut files, &mut bytes_total)?;

    fs::create_dir_all(destination).map_err(|err| OpsError::from_io(destination, err))?;
    for dir in &dirs {
        let relative = dir.strip_prefix(src).map_err(|_| OpsError::InvalidName {
            name: dir.display().to_string(),
        })?;
        let target = destination.join(relative);
        fs::create_dir_all(&target).map_err(|err| OpsError::from_io(&target, err))?;
    }

    let mut files_done = 0_u64;
    let mut bytes_done = 0_u64;
    for file in &files {
        check_cancel(cancel)?;
        let relative = file.strip_prefix(src).map_err(|_| OpsError::InvalidName {
            name: file.display().to_string(),
        })?;
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|err| OpsError::from_io(parent, err))?;
        }
        let copied = fs::copy(file, &target).map_err(|err| OpsError::from_io(file, err))?;
        bytes_done += copied;
        files_done += 1;
        bytes_total = bytes_total.max(bytes_done);
        on_progress(Progress {
            current: file.clone(),
            bytes_done,
            bytes_total,
            files_done,
            files_total: files.len().max(files_done as usize) as u64,
        });
    }

    Ok(())
}

fn walk(
    root: &Path,
    dirs: &mut Vec<PathBuf>,
    files: &mut Vec<PathBuf>,
    bytes_total: &mut u64,
) -> Result<(), OpsError> {
    let metadata = fs::symlink_metadata(root).map_err(|err| OpsError::from_io(root, err))?;
    if metadata.is_dir() {
        dirs.push(root.to_path_buf());
        let read_dir = fs::read_dir(root).map_err(|err| OpsError::from_io(root, err))?;
        for entry in read_dir {
            let entry = entry.map_err(|err| OpsError::from_io(root, err))?;
            walk(&entry.path(), dirs, files, bytes_total)?;
        }
    } else {
        *bytes_total += metadata.len();
        files.push(root.to_path_buf());
    }
    Ok(())
}

fn remove_source(src: &Path) -> Result<(), OpsError> {
    let metadata = fs::symlink_metadata(src).map_err(|err| OpsError::from_io(src, err))?;
    if metadata.is_dir() {
        fs::remove_dir_all(src).map_err(|err| OpsError::from_io(src, err))
    } else {
        fs::remove_file(src).map_err(|err| OpsError::from_io(src, err))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use super::{
        OpKind, Progress, copy_recursive, move_entry, rename_entry, undo, unique_destination,
    };
    use tempfile::TempDir;

    fn tree() -> TempDir {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        fs::create_dir_all(dir.path().join("origem/sub")).expect("cria subdiretório");
        fs::write(dir.path().join("origem/a.txt"), b"aaa").expect("cria a.txt");
        fs::write(dir.path().join("origem/sub/b.txt"), b"bbbbb").expect("cria b.txt");
        fs::create_dir(dir.path().join("destino")).expect("cria destino");
        dir
    }

    #[test]
    fn copy_recursive_reports_file_progress() {
        let dir = tree();
        let progress = Mutex::new(Vec::new());
        let destination = copy_recursive(
            dir.path().join("origem"),
            dir.path().join("destino"),
            &|update: Progress| progress.lock().expect("lock").push(update),
        )
        .expect("copia árvore");

        assert_eq!(destination, dir.path().join("destino/origem"));
        assert_eq!(
            fs::read(dir.path().join("destino/origem/sub/b.txt")).expect("lê b"),
            b"bbbbb"
        );

        let progress = progress.into_inner().expect("lock");
        assert_eq!(progress.len(), 2);
        let last = progress.last().expect("progresso");
        assert_eq!(last.files_done, 2);
        assert_eq!(last.files_total, 2);
        assert_eq!(last.bytes_done, 8);
        assert!(last.fraction() > 0.99);
    }

    #[test]
    fn copy_conflict_suffixes_destination() {
        let dir = tree();
        let destino = dir.path().join("destino");
        fs::write(destino.join("a.txt"), b"original").expect("cria conflito");

        let copied =
            copy_recursive(dir.path().join("origem/a.txt"), &destino, &|_| {}).expect("copia");
        assert_eq!(copied, destino.join("a (2).txt"));
        assert_eq!(
            fs::read(destino.join("a.txt")).expect("lê original"),
            b"original",
            "nunca sobrescreve"
        );
    }

    #[test]
    fn unique_destination_preserves_extension_and_increments() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        fs::write(dir.path().join("foto.png"), b"1").expect("cria foto.png");
        fs::write(dir.path().join("foto (2).png"), b"2").expect("cria foto (2).png");

        assert_eq!(
            unique_destination(dir.path(), "foto.png"),
            dir.path().join("foto (3).png")
        );
        assert_eq!(
            unique_destination(dir.path(), "novo"),
            dir.path().join("novo")
        );
    }

    #[test]
    fn move_entry_same_volume_removes_source() {
        let dir = tree();
        let source = dir.path().join("origem/a.txt");
        let moved = move_entry(&source, dir.path().join("destino"), &|_| {}).expect("move");

        assert_eq!(moved, dir.path().join("destino/a.txt"));
        assert!(!source.exists());
        assert_eq!(fs::read(&moved).expect("lê movido"), b"aaa");
    }

    #[test]
    fn rename_entry_changes_name_and_resolves_conflicts() {
        let dir = tree();
        let source = dir.path().join("origem/a.txt");
        let renamed = rename_entry(&source, "renomeado.txt").expect("renomeia");
        assert_eq!(renamed, dir.path().join("origem/renomeado.txt"));
        assert!(!source.exists());

        fs::write(dir.path().join("origem/a.txt"), b"novo").expect("cria a.txt");
        let conflicted = rename_entry(dir.path().join("origem/a.txt"), "renomeado.txt")
            .expect("renomeia com conflito");
        assert_eq!(conflicted, dir.path().join("origem/renomeado (2).txt"));
    }

    #[test]
    fn rename_rejects_path_separators() {
        let dir = tree();
        let err = rename_entry(dir.path().join("origem/a.txt"), "../fora.txt")
            .expect_err("nome inválido");
        assert!(matches!(err, super::OpsError::InvalidName { .. }));
    }

    #[test]
    fn undo_copy_deletes_destination_through_queue_op() {
        let dir = tree();
        let copied = copy_recursive(
            dir.path().join("origem"),
            dir.path().join("destino"),
            &|_| {},
        )
        .expect("copia");
        let completed = super::CompletedOp {
            kind: OpKind::Copy,
            source: dir.path().join("origem"),
            destination: copied.clone(),
        };
        let inverse = undo(&completed).expect("copy é desfazível");
        assert_eq!(inverse.kind, OpKind::Delete);
        assert_eq!(inverse.params.src, copied);
        assert!(inverse.params.dest.is_none());
    }

    #[test]
    fn undo_move_returns_to_source_directory() {
        let dir = tree();
        let source = dir.path().join("origem/a.txt");
        let moved = move_entry(&source, dir.path().join("destino"), &|_| {}).expect("move");
        let completed = super::CompletedOp {
            kind: OpKind::Move,
            source: source.clone(),
            destination: moved.clone(),
        };
        let inverse = undo(&completed).expect("move é desfazível");
        assert_eq!(inverse.kind, OpKind::Move);
        assert_eq!(inverse.params.src, moved);
        assert_eq!(
            inverse.params.dest.as_deref(),
            source.parent(),
            "volta para o diretório de origem"
        );
    }

    #[test]
    fn undo_rename_reverts_to_original_name() {
        let dir = tree();
        let source = dir.path().join("origem/a.txt");
        let renamed = rename_entry(&source, "b.txt").expect("renomeia");
        let completed = super::CompletedOp {
            kind: OpKind::Rename,
            source: source.clone(),
            destination: renamed.clone(),
        };
        let inverse = undo(&completed).expect("rename é desfazível");
        assert_eq!(inverse.kind, OpKind::Rename);
        assert_eq!(inverse.params.src, renamed);
        assert_eq!(
            inverse.params.dest.as_deref(),
            Some(std::path::Path::new("a.txt"))
        );
    }

    #[test]
    fn delete_is_never_undoable() {
        let completed = super::CompletedOp {
            kind: OpKind::Delete,
            source: std::path::PathBuf::from("/tmp/x"),
            destination: std::path::PathBuf::from("/tmp/x"),
        };
        assert!(undo(&completed).is_none());
    }

    // `EXDEV` real exige dois filesystems: não é simulável de forma portátil
    // no CI. O caminho de cópia + remoção é o mesmo de `copy_recursive`, já
    // coberto acima; a decisão de roteamento fica documentada no código.
    // `trash::delete` também não é testado no CI: sem sessão gráfica/portal o
    // backend da lixeira falha. A decisão `Delete → lixeira` está isolada em
    // `delete_to_trash`.
}
