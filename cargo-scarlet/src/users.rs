//! Guest identities belong to the image, independently of the builder's UID.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use tar::{Builder, EntryType, Header};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct User {
    name: String,
    uid: u32,
    gid: Option<u32>,
    #[serde(default)]
    groups: Vec<u32>,
    password_hash_file: Option<PathBuf>,
}

#[derive(Default)]
pub(super) struct Accounts {
    homes: BTreeMap<PathBuf, (u32, u32)>,
    records: String,
}

impl Accounts {
    pub(super) fn prepare(
        users: &[User],
        console_root: bool,
        project: &Path,
        staging: &Path,
    ) -> Result<Self, String> {
        let mut accounts = Accounts::default();
        let mut uids = BTreeSet::new();
        for user in users {
            let name = &user.name;
            if name.is_empty()
                || name.len() > 32
                || !name.as_bytes()[0].is_ascii_lowercase()
                || !name.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_')
                })
            {
                return Err("invalid console account name".into());
            }
            let gid = user.gid.unwrap_or(user.uid);
            let root = name == "root";
            if (user.uid == 0) != root
                || (gid == 0) != root
                || user.uid == u32::MAX
                || gid == u32::MAX
                || user.groups.iter().any(|&id| id == 0 || id == u32::MAX)
            {
                return Err(format!("invalid UID or GID for {name}"));
            }
            let home = if root {
                PathBuf::from("root")
            } else {
                Path::new("home").join(name)
            };
            if !uids.insert(user.uid) || accounts.homes.insert(home, (user.uid, gid)).is_some() {
                return Err(format!("duplicate console account name or UID for {name}"));
            }
            let hash = match &user.password_hash_file {
                None => "!".into(),
                Some(path) => {
                    let hash = fs::read_to_string(project.join(path))
                        .map_err(|e| format!("cannot read password-hash-file for {name}: {e}"))?;
                    let hash = hash.trim_end_matches(['\r', '\n']);
                    if hash.len() != 60
                        || !(hash.starts_with("$2b$12$") || hash.starts_with("$2y$12$"))
                        || !hash[7..]
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'/'))
                    {
                        return Err(format!("expected one bcrypt cost-12 verifier for {name}"));
                    }
                    hash.to_owned()
                }
            };
            let groups = user
                .groups
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            use std::fmt::Write as _;
            writeln!(
                &mut accounts.records,
                "{name}:{}:{gid}:{groups}:{hash}",
                user.uid
            )
            .unwrap();
        }
        if console_root && !uids.contains(&0) {
            accounts.homes.insert(PathBuf::from("root"), (0, 0));
            accounts.records.push_str("root:0:0::\n");
        }
        if accounts.records.len() > 65536 {
            return Err("console account database exceeds 64 KiB".into());
        }
        let db = staging.join("etc/scarlet-users");
        super::ensure_no_symlink_ancestors(staging, &db)?;
        if db.exists() {
            return Err("image users conflict with a layer providing /etc/scarlet-users".into());
        }
        for dir in [PathBuf::from("etc"), PathBuf::from("home")]
            .into_iter()
            .chain(accounts.homes.keys().cloned())
        {
            let path = staging.join(dir);
            super::ensure_no_symlink_ancestors(staging, &path)?;
            fs::create_dir_all(&path)
                .map_err(|e| format!("cannot stage {}: {e}", path.display()))?;
        }
        Ok(accounts)
    }

    /// Stream the same guest tree to the content hash and to mke2fs. Neither
    /// the staging directory nor the host needs chown or a cleartext password.
    pub(super) fn archive(&self, root: &Path, out: impl Write) -> io::Result<()> {
        let mut tar = Builder::new(out);
        let mut links = HashMap::new();
        self.tree(root, Path::new(""), &mut tar, &mut links)?;
        let mut header = Header::new_gnu();
        header.set_mode(0o600);
        header.set_entry_type(EntryType::Regular);
        header.set_size(self.records.len() as u64);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        tar.append_data(&mut header, "etc/scarlet-users", self.records.as_bytes())?;
        tar.into_inner()?.flush()
    }

    fn tree<W: Write>(
        &self,
        root: &Path,
        dir: &Path,
        tar: &mut Builder<W>,
        links: &mut HashMap<(u64, u64, u32, u32), PathBuf>,
    ) -> io::Result<()> {
        let mut entries = fs::read_dir(root.join(dir))?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = dir.join(entry.file_name());
            let meta = fs::symlink_metadata(entry.path())?;
            let home = path
                .ancestors()
                .find_map(|ancestor| self.homes.get(ancestor));
            let (uid, gid) = home.copied().unwrap_or((0, 0));
            let mut h = Header::new_gnu();
            h.set_metadata(&meta);
            h.set_uid(uid.into());
            h.set_gid(gid.into());
            h.set_mtime(0);
            if meta.is_dir() {
                if self.homes.contains_key(&path) {
                    h.set_mode(0o700);
                }
                if path == Path::new("etc") || path == Path::new("home") {
                    h.set_mode(0o755);
                }
                h.set_size(0);
                tar.append_data(&mut h, &path, io::empty())?;
                self.tree(root, &path, tar, links)?;
            } else if meta.is_symlink() {
                h.set_size(0);
                tar.append_link(&mut h, &path, fs::read_link(entry.path())?)?;
            } else if meta.is_file() {
                if meta.nlink() > 1 {
                    let key = (meta.dev(), meta.ino(), uid, gid);
                    if let Some(first) = links.get(&key) {
                        h.set_entry_type(EntryType::Link);
                        h.set_size(0);
                        tar.append_link(&mut h, &path, first)?;
                        continue;
                    }
                    links.insert(key, path.clone());
                }
                tar.append_data(&mut h, &path, fs::File::open(entry.path())?)?;
            } else {
                return Err(io::Error::other(format!(
                    "unsupported staging entry {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }
}
