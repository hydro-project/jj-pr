use std::borrow::Borrow;
use std::fmt::{self, Display, Write};
use std::ops::Deref;

use ref_cast::{RefCastCustom, ref_cast_custom};
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
        let name = self.0.as_ref();
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
            #[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, RefCastCustom)]
            #[repr(transparent)]
            #[serde(transparent)]
            $vis struct $Name<T: ?Sized + AsRef<str> = String>(pub T);

            impl<T: ?Sized + AsRef<str>> AsRef<str> for $Name<T> {
                fn as_ref(&self) -> &str {
                    self.0.as_ref()
                }
            }

            impl<T: ?Sized + AsRef<str>> Display for $Name<T> {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    self.0.as_ref().fmt(f)
                }
            }

            // `Deref` bound ensure no deref-to-self infinite recursion.
            impl<T: ?Sized + AsRef<str> + Deref<Target = str>> Deref for $Name<T> {
                type Target = $Name<str>;
                fn deref(&self) -> &Self::Target {
                    $Name::from_str(self.0.as_ref())
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

            impl<Lhs: ?Sized + AsRef<str>, Rhs: ?Sized + AsRef<str>> PartialEq<$Name<Rhs>> for $Name<Lhs> {
                fn eq(&self, other: &$Name<Rhs>) -> bool {
                    self.as_ref() == other.as_ref()
                }
            }

            impl<Lhs: ?Sized + AsRef<str>, Rhs: ?Sized + AsRef<str>> PartialOrd<$Name<Rhs>> for $Name<Lhs> {
                fn partial_cmp(&self, other: &$Name<Rhs>) -> Option<::core::cmp::Ordering> {
                    self.as_ref().partial_cmp(other.as_ref())
                }
            }

            impl<T: ?Sized + AsRef<str>> $Name<T> {
                /// Returns the inner `&str`, for passing to external APIs that require it.
                pub fn as_str(&self) -> &str {
                    self.as_ref()
                }
            }

            impl $Name<str> {
                /// Cast `&str` to `&Name<str>`.
                #[ref_cast_custom]
                pub const fn from_str(s: &str) -> &Self;
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

    /// A git remote name (e.g. "origin", "fork").
    pub Remote;

    /// A GitHub owner (user or organization, e.g. "hydro-project").
    pub Owner;
}

/// The `@git` tracking remote (local-only, not a real push target).
pub const REMOTE_GIT: &Remote<str> = Remote::from_str("git");

/// The default remote name.
pub const REMOTE_ORIGIN: &Remote<str> = Remote::from_str("origin");
