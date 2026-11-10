//! Integração Git do Arandu.
//!
//! Este crate expõe a primeira fatia da integração Git: descoberta de
//! repositório, leitura de status, staging/unstaging e criação de commits.
//! Tudo é implementado com [`gix`] (gitoxide), sem depender do binário `git`
//! em tempo de execução.
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

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, BString, ByteSlice};
use gix::index::entry::Mode as IndexMode;
use gix::objs::tree::{EntryKind, EntryMode};

/// Máximo de commits visitados ao calcular `ahead`/`behind` de cada lado.
const MAX_GRAPH_WALK: usize = 10_000;

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

fn sort_dedup(entries: &mut Vec<FileEntry>) {
    entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.status.cmp(&b.status)));
    entries.dedup();
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
}
