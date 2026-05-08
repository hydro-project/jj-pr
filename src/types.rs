use std::borrow::Borrow;
use std::fmt::{self, Display, Write};
use std::ops::Deref;

use ref_cast::RefCast;
use serde::{Deserialize, Serialize};

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

impl<T: ?Sized + Display> AsRevset for CommitId<T> {
    fn as_revset(&self) -> Revset {
        Revset(format!("commit_id({})", &self.0))
    }
}

impl<T: ?Sized + Display> AsRevset for ChangeId<T> {
    fn as_revset(&self) -> Revset {
        Revset(format!("change_id({})", &self.0))
    }
}

impl<T: ?Sized + Display> AsRevset for Bookmark<T> {
    fn as_revset(&self) -> Revset {
        // Quote the bookmark name to handle special characters (e.g. `-`, `/`).
        let name = self.0.to_string();
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

/// Generates a transparent newtype over a string-like inner type with `Display`,
/// `Deref`, `Borrow`, and `ToOwned` impls (mirroring the `str`/`String` pattern).
macro_rules! newtype_str {
    (
        $(
            $( #[$meta:meta] )* $vis:vis $Name:ident;
        )*
    ) => {
        $(
            $( #[$meta] )*
            #[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, PartialEq, PartialOrd, RefCast)]
            #[repr(transparent)]
            #[serde(transparent)]
            $vis struct $Name<T: ?Sized = String>(pub T);

            impl<T: ?Sized + Display> Display for $Name<T> {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    self.0.fmt(f)
                }
            }

            impl<T: ?Sized + Deref> Deref for $Name<T> {
                type Target = $Name<T::Target>;
                fn deref(&self) -> &Self::Target {
                    $Name::ref_cast(self.0.deref())
                }
            }

            impl Borrow<$Name<str>> for $Name {
                fn borrow(&self) -> &$Name<str> {
                    self
                }
            }

            impl ToOwned for $Name<str> {
                type Owned = $Name<String>;
                fn to_owned(&self) -> Self::Owned {
                    $Name(self.0.to_owned())
                }
            }

            impl $Name<str> {
                /// Returns the inner `&str`, for passing to external APIs that require it.
                pub fn as_str(&self) -> &str {
                    &self.0
                }
            }
        )*
    };
}

newtype_str! {
    /// A `git`/`jj` commit SHA hash. Used by both `jj` and GitHub.
    pub CommitId;

    /// A `jj` change ID.
    pub ChangeId;

    /// A bookmark (branch) name.
    pub Bookmark;
}
