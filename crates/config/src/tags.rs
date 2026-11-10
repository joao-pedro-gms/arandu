//! Tags de arquivos persistidas em TOML.
//!
//! O armazenamento é intencionalmente simples: um mapa de caminho absoluto
//! para um conjunto ordenado de tags, gravado em `<config_dir>/tags.toml`
//! (mesmo diretório de `config.toml` e `keymap.toml`). Um banco de dados pode
//! substituir esta implementação na fase 2 sem mudar a API pública.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Config, ConfigError};

/// Tamanho máximo de uma tag, em caracteres.
const MAX_TAG_LEN: usize = 32;

/// Tags por arquivo, com persistência em TOML.
///
/// A chave é o caminho absoluto canônico (quando o arquivo existe) ou uma
/// versão absoluta lexicalmente normalizada (quando ainda não existe). As tags
/// de cada arquivo são mantidas ordenadas e sem duplicatas exatas; o caso é
/// preservado.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagsStore {
    /// Mapa caminho → tags ordenadas.
    #[serde(default)]
    tags: BTreeMap<String, BTreeSet<String>>,
}

impl TagsStore {
    /// Caminho padrão do arquivo de tags.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        Ok(Config::config_dir()?.join("tags.toml"))
    }

    /// Carrega o store do caminho padrão; arquivo inexistente vira store vazio.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(Self::default_path()?)
    }

    /// Carrega o store de um caminho específico; arquivo inexistente vira
    /// store vazio.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }

    /// Grava no caminho padrão, criando os diretórios necessários.
    pub fn save(&self) -> Result<PathBuf, ConfigError> {
        let path = Self::default_path()?;
        self.save_to(&path)?;
        Ok(path)
    }

    /// Grava em um caminho específico, criando os diretórios necessários.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Tags de um caminho, em ordem.
    pub fn tags_of(&self, path: impl AsRef<Path>) -> Vec<String> {
        self.tags
            .get(&path_key(path.as_ref()))
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Adiciona uma tag a um caminho.
    ///
    /// A tag é validada (ver [`ConfigError::InvalidTag`]) e normalizada com
    /// `trim`; duplicatas exatas são ignoradas.
    pub fn add(&mut self, path: impl AsRef<Path>, tag: &str) -> Result<(), ConfigError> {
        let tag = validate_tag(tag)?;
        self.tags
            .entry(path_key(path.as_ref()))
            .or_default()
            .insert(tag);
        Ok(())
    }

    /// Remove uma tag de um caminho, se existir.
    pub fn remove(&mut self, path: impl AsRef<Path>, tag: &str) {
        let tag = tag.trim();
        let key = path_key(path.as_ref());
        let Some(tags) = self.tags.get_mut(&key) else {
            return;
        };
        tags.remove(tag);
        if tags.is_empty() {
            self.tags.remove(&key);
        }
    }

    /// Remove uma tag de todos os caminhos (para o futuro gerenciador de tags).
    pub fn remove_tag_everywhere(&mut self, tag: &str) {
        let tag = tag.trim().to_owned();
        self.tags.retain(|_, tags| {
            tags.remove(&tag);
            !tags.is_empty()
        });
    }

    /// Todas as tags conhecidas, em ordem.
    pub fn all_tags(&self) -> Vec<String> {
        let mut all = BTreeSet::new();
        for tags in self.tags.values() {
            all.extend(tags.iter().cloned());
        }
        all.into_iter().collect()
    }

    /// Remove todas as tags em memória (não persiste).
    pub fn clear(&mut self) {
        self.tags.clear();
    }
}

/// Valida e normaliza uma tag.
fn validate_tag(tag: &str) -> Result<String, ConfigError> {
    let tag = tag.trim();
    let invalid = tag.is_empty()
        || tag.contains(',')
        || tag.contains('\n')
        || tag.contains('\r')
        || tag.chars().count() > MAX_TAG_LEN;
    if invalid {
        return Err(ConfigError::InvalidTag {
            tag: tag.to_owned(),
        });
    }
    Ok(tag.to_owned())
}

/// Chave canônica de um caminho.
///
/// Usa `canonicalize` quando o caminho existe; caso contrário, absolutiza em
/// relação ao diretório atual e resolve `.`/`..` de forma lexical.
fn path_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .map(|canonical| canonical.to_string_lossy().into_owned())
        .unwrap_or_else(|_| lexical_absolute(path).to_string_lossy().into_owned())
}

fn lexical_absolute(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };

    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::TagsStore;

    fn temp_file(dir: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, b"conteudo").expect("cria arquivo");
        path
    }

    #[test]
    fn roundtrips_through_toml() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let file = temp_file(&dir, "a.txt");
        let tags_path = dir.path().join("tags.toml");

        let mut store = TagsStore::default();
        store.add(&file, "importante").expect("adiciona tag");
        store.add(&file, "revisar").expect("adiciona tag");
        store.save_to(&tags_path).expect("salva tags");

        let loaded = TagsStore::load_from(&tags_path).expect("carrega tags");
        assert_eq!(
            loaded.tags_of(&file),
            vec!["importante".to_owned(), "revisar".to_owned()]
        );
    }

    #[test]
    fn add_remove_and_tags_of() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let file = temp_file(&dir, "b.txt");

        let mut store = TagsStore::default();
        store.add(&file, "beta").expect("adiciona tag");
        store.add(&file, "alfa").expect("adiciona tag");
        store.add(&file, "alfa").expect("duplicata é ignorada");
        assert_eq!(
            store.tags_of(&file),
            vec!["alfa".to_owned(), "beta".to_owned()]
        );

        store.remove(&file, "alfa");
        assert_eq!(store.tags_of(&file), vec!["beta".to_owned()]);

        store.remove(&file, "beta");
        assert!(store.tags_of(&file).is_empty());
    }

    #[test]
    fn rejects_empty_tag() {
        let mut store = TagsStore::default();
        let error = store
            .add("x.txt", "   ")
            .expect_err("tag vazia deve falhar");
        assert!(
            matches!(error, super::ConfigError::InvalidTag { .. }),
            "erro inesperado: {error:?}"
        );
    }

    #[test]
    fn rejects_tag_with_comma() {
        let mut store = TagsStore::default();
        let error = store
            .add("x.txt", "a,b")
            .expect_err("tag com vírgula deve falhar");
        assert!(
            matches!(error, super::ConfigError::InvalidTag { .. }),
            "erro inesperado: {error:?}"
        );
    }

    #[test]
    fn rejects_tag_longer_than_limit() {
        let mut store = TagsStore::default();
        let long = "x".repeat(super::MAX_TAG_LEN + 1);
        let error = store
            .add("x.txt", &long)
            .expect_err("tag longa deve falhar");
        assert!(
            matches!(error, super::ConfigError::InvalidTag { .. }),
            "erro inesperado: {error:?}"
        );
    }

    #[test]
    fn remove_tag_everywhere_drops_empty_entries() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let a = temp_file(&dir, "a.txt");
        let b = temp_file(&dir, "b.txt");

        let mut store = TagsStore::default();
        store.add(&a, "comum").expect("tag em a");
        store.add(&a, "so-a").expect("tag em a");
        store.add(&b, "comum").expect("tag em b");

        store.remove_tag_everywhere("comum");
        assert_eq!(store.tags_of(&a), vec!["so-a".to_owned()]);
        assert!(store.tags_of(&b).is_empty());
        assert_eq!(store.all_tags(), vec!["so-a".to_owned()]);
    }

    #[test]
    fn missing_file_is_an_empty_store() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let loaded = TagsStore::load_from(dir.path().join("nao-existe.toml"))
            .expect("arquivo inexistente não é erro");
        assert!(loaded.all_tags().is_empty());
    }

    #[test]
    fn relative_and_absolute_paths_share_a_key() {
        let cwd = std::env::current_dir().expect("diretório atual");
        let absolute = cwd.join("tags-fixture/sub/../file.txt");
        let relative = std::path::PathBuf::from("tags-fixture/file.txt");

        let mut store = TagsStore::default();
        store
            .add(&relative, "nota")
            .expect("adiciona por caminho relativo");
        assert_eq!(store.tags_of(&absolute), vec!["nota".to_owned()]);
    }
}
