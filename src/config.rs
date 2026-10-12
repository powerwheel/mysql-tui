use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// SSH jump host ("bastion") settings for tunnelled connections.
/// The MySQL host/port of the connection are resolved from the jump host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SshConfig {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// Login user on the jump host; empty = current OS user.
    #[serde(default)]
    pub user: String,
    /// Private key path; empty = ssh agent / default keys (~/.ssh/id_*).
    #[serde(default)]
    pub key_path: String,
}

fn default_ssh_port() -> u16 {
    22
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: default_ssh_port(),
            user: String::new(),
            key_path: String::new(),
        }
    }
}

/// TLS negotiation policy, mirroring the mysql CLI `--ssl-mode` levels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// Plain TCP.
    #[default]
    Off,
    /// Encrypt, but accept any server certificate.
    Required,
    /// Encrypt and verify the certificate chain (system roots or `ca_path`), ignore hostname.
    VerifyCa,
    /// Encrypt, verify the chain and the hostname.
    VerifyIdentity,
}

impl TlsMode {
    pub const ALL: [TlsMode; 4] = [
        TlsMode::Off,
        TlsMode::Required,
        TlsMode::VerifyCa,
        TlsMode::VerifyIdentity,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TlsMode::Off => "off",
            TlsMode::Required => "required",
            TlsMode::VerifyCa => "verify-ca",
            TlsMode::VerifyIdentity => "verify-identity",
        }
    }

    pub fn parse(s: &str) -> Option<TlsMode> {
        TlsMode::ALL.into_iter().find(|m| m.as_str() == s.trim())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TlsConfig {
    #[serde(default)]
    pub mode: TlsMode,
    /// Optional CA certificate (PEM or DER); empty = system trust store.
    #[serde(default)]
    pub ca_path: String,
}

/// A saved MySQL connection profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Connection {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
    /// Database selected on connect; may be empty.
    #[serde(default)]
    pub database: String,
    /// When set (and host non-empty), connect through an SSH tunnel.
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    #[serde(default)]
    pub tls: TlsConfig,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3306
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            name: "New connection".to_string(),
            host: default_host(),
            port: default_port(),
            user: "root".to_string(),
            password: String::new(),
            database: String::new(),
            ssh: None,
            tls: TlsConfig::default(),
        }
    }
}

impl Connection {
    /// True when this connection should be established through an SSH tunnel.
    pub fn has_ssh(&self) -> bool {
        self.ssh.as_ref().is_some_and(|s| !s.host.trim().is_empty())
    }

    pub fn has_tls(&self) -> bool {
        self.tls.mode != TlsMode::Off
    }

    /// Redacted description shown in the connection list.
    pub fn describe(&self) -> String {
        let db = if self.database.is_empty() {
            "-"
        } else {
            &self.database
        };
        let mut s = format!("{}@{}:{}/{}", self.user, self.host, self.port, db);
        if self.has_ssh() {
            s.push_str(" [ssh]");
        }
        if self.has_tls() {
            s.push_str(" [tls]");
        }
        s
    }

    /// Display name: falls back to the user@host description when unnamed.
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            "Unnamed"
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub connections: Vec<Connection>,
    #[serde(default = "default_selected")]
    pub selected: Option<usize>,
    /// Index of the connection to auto-connect to on startup, if any.
    /// Stored as a 1-based index so that `0` (or absence) means disabled.
    #[serde(default, with = "auto_connect_index")]
    pub auto_connect: Option<usize>,
    /// Default LIMIT appended to SELECT statements that lack one. 0 disables.
    #[serde(default = "default_limit")]
    pub default_limit: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connections: Vec::new(),
            selected: None,
            auto_connect: None,
            default_limit: default_limit(),
        }
    }
}

fn default_limit() -> usize {
    1000
}

fn default_selected() -> Option<usize> {
    None
}

mod auto_connect_index {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &Option<usize>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.map(|i| i + 1).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Option::<usize>::deserialize(deserializer)?;
        Ok(v.and_then(|i| if i == 0 { None } else { Some(i - 1) }))
    }
}

/// ~/.config/mysql-tui/connections.toml
pub fn config_path() -> PathBuf {
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("mysql-tui");
    p.push("connections.toml");
    p
}

/// Encrypted on-disk format: magic_number identifies the file as encrypted,
/// salt/nonce/ciphertext are standard-base64 encoded.
#[derive(Debug, Serialize, Deserialize)]
struct EncryptedFile {
    magic_number: String,
    salt: String,
    nonce: String,
    ciphertext: String,
}

const ENCRYPTED_MAGIC: &str = "encrypted_v1";

/// Outcome of [`Config::load`].
pub enum ConfigLoadResult {
    /// File is encrypted; caller must prompt for a password.
    Encrypted {
        salt: String,
        nonce: String,
        ciphertext: String,
    },
    /// No config file exists — first run, user should set a password.
    Fresh,
    /// A plaintext file exists but has never been encrypted — user should set a password.
    PlaintextNeedsEncryption(Config),
}

impl Config {
    pub fn load() -> Result<ConfigLoadResult, String> {
        let path = config_path();
        if !path.exists() {
            return Ok(ConfigLoadResult::Fresh);
        }
        let text = fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;

        // Try encrypted format first
        if let Ok(enc) = toml::from_str::<EncryptedFile>(&text)
            && enc.magic_number == ENCRYPTED_MAGIC
        {
            return Ok(ConfigLoadResult::Encrypted {
                salt: enc.salt,
                nonce: enc.nonce,
                ciphertext: enc.ciphertext,
            });
        }

        // Fall back to plaintext
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        let cfg = cfg.sanitize();
        Ok(ConfigLoadResult::PlaintextNeedsEncryption(cfg))
    }

    /// Save to disk. If `password` is `Some`, the file is written encrypted;
    /// otherwise it is written as plaintext (only used for initial/testing scenarios).
    pub fn save(&self, password: Option<&str>) -> Result<(), String> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        }

        let text =
            toml::to_string_pretty(self).map_err(|e| format!("failed to serialize config: {e}"))?;

        let output = if let Some(pw) = password {
            // Read existing salt or generate a new one
            let salt = read_existing_salt().unwrap_or_else(crate::crypto::generate_salt);
            let (nonce, ciphertext) = crate::crypto::encrypt(pw, &salt, text.as_bytes())?;
            let enc = EncryptedFile {
                magic_number: ENCRYPTED_MAGIC.to_string(),
                salt,
                nonce,
                ciphertext,
            };
            toml::to_string_pretty(&enc)
                .map_err(|e| format!("failed to serialize encrypted config: {e}"))?
        } else {
            text
        };

        fs::write(&path, output).map_err(|e| format!("failed to write {}: {e}", path.display()))
    }

    fn sanitize(mut self) -> Self {
        let len = self.connections.len();
        self.selected = self.selected.filter(|i| *i < len);
        self.auto_connect = self.auto_connect.filter(|i| *i < len);
        self
    }

    /// Serialize the config to TOML bytes (used by crypto module for encryption).
    #[cfg(test)]
    pub fn to_toml_bytes(&self) -> Result<Vec<u8>, String> {
        toml::to_string_pretty(self)
            .map(|s| s.into_bytes())
            .map_err(|e| format!("failed to serialize config: {e}"))
    }

    /// Parse a config from decrypted TOML bytes.
    pub fn from_toml_bytes(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes)
            .map_err(|e| format!("invalid UTF-8 in decrypted config: {e}"))?;
        let cfg: Config =
            toml::from_str(text).map_err(|e| format!("failed to parse decrypted config: {e}"))?;
        Ok(cfg.sanitize())
    }
}

/// Read the salt from the existing encrypted config file, if present.
/// Reusing the salt avoids re-deriving a different key on each save.
fn read_existing_salt() -> Option<String> {
    let path = config_path();
    let text = fs::read_to_string(&path).ok()?;
    let enc: EncryptedFile = toml::from_str(&text).ok()?;
    if enc.magic_number == ENCRYPTED_MAGIC {
        Some(enc.salt)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            connections: vec![Connection {
                name: "local".into(),
                host: "127.0.0.1".into(),
                port: 3306,
                user: "root".into(),
                password: "secret".into(),
                database: "test".into(),
                ssh: None,
                tls: TlsConfig::default(),
            }],
            selected: Some(0),
            auto_connect: None,
            default_limit: 1000,
        }
    }

    #[test]
    fn roundtrip_toml() {
        let cfg = sample();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.connections, cfg.connections);
        assert_eq!(back.selected, Some(0));
    }

    #[test]
    fn parse_defaults_and_invalid_index() {
        let text = r#"
[[connections]]
name = "a"
user = "u"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.connections.len(), 1);
        assert_eq!(cfg.connections[0].host, "127.0.0.1");
        assert_eq!(cfg.connections[0].port, 3306);
        let cfg = Config {
            selected: Some(9),
            ..cfg
        };
        let cfg = cfg.sanitize();
        assert_eq!(cfg.selected, None);
    }

    #[test]
    fn describe_and_display_name() {
        let c = Connection::default();
        assert_eq!(c.describe(), "root@127.0.0.1:3306/-");
        assert_eq!(c.display_name(), "New connection");
        let c = Connection {
            name: String::new(),
            ..Connection::default()
        };
        assert_eq!(c.display_name(), "Unnamed");
        let c = Connection {
            name: "prod".into(),
            database: "shop".into(),
            ..Connection::default()
        };
        assert_eq!(c.describe(), "root@127.0.0.1:3306/shop");
        assert_eq!(c.display_name(), "prod");
    }

    #[test]
    fn encrypted_roundtrip() {
        let cfg = sample();
        let password = "test-password-123";
        let bytes = cfg.to_toml_bytes().unwrap();
        let salt = crate::crypto::generate_salt();
        let (nonce, ct) = crate::crypto::encrypt(password, &salt, &bytes).unwrap();
        let decrypted = crate::crypto::decrypt(password, &salt, &nonce, &ct).unwrap();
        let back = Config::from_toml_bytes(&decrypted).unwrap();
        assert_eq!(back.connections, cfg.connections);
        assert_eq!(back.selected, Some(0));
        assert_eq!(back.default_limit, 1000);
    }

    #[test]
    fn ssh_roundtrip_and_defaults() {
        let cfg = Config {
            connections: vec![Connection {
                ssh: Some(SshConfig {
                    host: "bastion.example.com".into(),
                    port: 22,
                    user: "deploy".into(),
                    key_path: "~/.ssh/id_ed25519".into(),
                }),
                ..Connection::default()
            }],
            ..Config::default()
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.connections[0].ssh, cfg.connections[0].ssh);
        assert!(back.connections[0].has_ssh());

        // legacy config without the ssh field still parses
        let legacy = r#"
[[connections]]
name = "a"
user = "u"
"#;
        let cfg: Config = toml::from_str(legacy).unwrap();
        assert_eq!(cfg.connections[0].ssh, None);
        assert!(!cfg.connections[0].has_ssh());
    }

    #[test]
    fn ssh_partial_fields_default() {
        let text = r#"
[[connections]]
name = "a"
user = "u"

[connections.ssh]
host = "jump"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        let ssh = cfg.connections[0].ssh.as_ref().unwrap();
        assert_eq!(ssh.host, "jump");
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.user, "");
        assert_eq!(ssh.key_path, "");
    }

    #[test]
    fn describe_marks_ssh() {
        let c = Connection {
            ssh: Some(SshConfig {
                host: "jump".into(),
                ..SshConfig::default()
            }),
            ..Connection::default()
        };
        assert!(c.describe().ends_with(" [ssh]"));
        // ssh block with empty host is treated as disabled
        let c = Connection {
            ssh: Some(SshConfig::default()),
            ..Connection::default()
        };
        assert!(!c.describe().contains("[ssh]"));
        assert!(!c.has_ssh());
    }

    #[test]
    fn tls_roundtrip_and_legacy_default() {
        let cfg = Config {
            connections: vec![Connection {
                tls: TlsConfig {
                    mode: TlsMode::VerifyCa,
                    ca_path: "/etc/ssl/rds-ca.pem".into(),
                },
                ..Connection::default()
            }],
            ..Config::default()
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(text.contains("mode = \"verify-ca\""));
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.connections[0].tls, cfg.connections[0].tls);
        assert!(back.connections[0].has_tls());
        assert!(back.connections[0].describe().ends_with(" [tls]"));

        let legacy = r#"
[[connections]]
name = "a"
"#;
        let cfg: Config = toml::from_str(legacy).unwrap();
        assert_eq!(cfg.connections[0].tls.mode, TlsMode::Off);
        assert!(!cfg.connections[0].has_tls());
        assert!(!cfg.connections[0].describe().contains("[tls]"));
    }

    #[test]
    fn tls_mode_parse_roundtrip() {
        for m in TlsMode::ALL {
            assert_eq!(TlsMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(TlsMode::parse(" required "), Some(TlsMode::Required));
        assert_eq!(TlsMode::parse("bogus"), None);
    }
}

