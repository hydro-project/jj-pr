use std::borrow::Borrow;
use std::fmt::Display;
use std::ops::Deref;

use ref_cast::RefCast;
use serde::{Deserialize, Serialize};

/// Newtype for a git commit SHA hash. Used by both jj and GitHub.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, Hash, Ord, PartialEq, PartialOrd, RefCast)]
#[repr(transparent)]
#[serde(transparent)]
pub struct CommitId<T: ?Sized = String>(pub T);

impl<T: ?Sized> Display for CommitId<T>
where
    T: Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: ?Sized> Deref for CommitId<T>
where
    T: Deref,
{
    type Target = CommitId<T::Target>;
    fn deref(&self) -> &Self::Target {
        CommitId::ref_cast(self.0.deref())
    }
}

impl Borrow<CommitId<str>> for CommitId {
    fn borrow(&self) -> &CommitId<str> {
        self
    }
}

impl ToOwned for CommitId<str> {
    type Owned = CommitId<String>;

    fn to_owned(&self) -> Self::Owned {
        CommitId(self.0.to_owned())
    }
}
