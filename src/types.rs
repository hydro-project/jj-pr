use std::fmt::{self, Display, Write};

/// A jj revset expression. Wraps a `String` that can be passed directly to `-r`.
pub struct Revset(String);

impl Revset {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for Revset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Convert a typed ID into a jj revset expression.
pub trait AsRevset {
    fn as_revset(&self) -> Revset;
}

impl<T: ?Sized + AsRef<str>> AsRevset for CommitId<T> {
    fn as_revset(&self) -> Revset {
        Revset(format!("commit_id({})", self.as_str()))
    }
}

impl<T: ?Sized + AsRef<str>> AsRevset for ChangeId<T> {
    fn as_revset(&self) -> Revset {
        Revset(format!("change_id({})", self.as_str()))
    }
}

impl<T: ?Sized + AsRef<str>> AsRevset for Bookmark<T> {
    fn as_revset(&self) -> Revset {
        // Quote the bookmark name to handle special characters (e.g. `-`, `/`).
        let name = self.as_str();
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        Revset(format!("bookmark(\"{}\")", escaped))
    }
}

impl<R: AsRevset + ?Sized> AsRevset for &R {
    fn as_revset(&self) -> Revset {
        (**self).as_revset()
    }
}

/// Join multiple revset-able items with `|` into a single revset expression.
pub fn revset_union(items: impl IntoIterator<Item = impl AsRevset>) -> Revset {
    let mut buf = String::new();
    for item in items {
        if !buf.is_empty() {
            buf.push_str(" | ");
        }
        write!(buf, "{}", item.as_revset()).unwrap();
    }
    Revset(buf)
}

strkind::strkind! {
    /// A `git`/`jj` commit SHA hash. Used by both `jj` and GitHub.
    pub CommitId;

    /// A `jj` change ID.
    pub ChangeId;

    /// A bookmark (branch) name.
    pub Bookmark;

    /// A git remote name (e.g. "origin", "fork").
    pub Remote;

    /// A GitHub owner (user or organization, e.g. "hydro-project").
    pub Owner;
}

/// The `@git` tracking remote (local-only, not a real push target).
pub const REMOTE_GIT: &Remote<str> = Remote::from_ref("git");

/// The default remote name.
pub const REMOTE_ORIGIN: &Remote<str> = Remote::from_ref("origin");
