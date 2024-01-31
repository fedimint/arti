//! State helper utility
//!
//! All the methods in this module perform appropriate mistrust checks.
//!
//! All the methods arrange to ensure suitably-finegrained exclusive access.
//! "Read-only" or "shared" mode is not supported.
//!
//! ### Differences from `tor_persist::StorageHandle`
//!
//!  * Explicit provision is made for multiple instances of a single facility.
//!    For example, multiple hidden services,
//!    each with their own state, and own lock.
//!
//!  * Locking (via filesystem locks) is mandatory, rather than optional -
//!    there is no "shared" mode.
//!
//!  * Locked state is represented in the Rust type system.
//!
//!  * We don't use traits to support multiple implementations.
//!    Platform support would be done in the future with `#[cfg]`.
//!    Testing is done by temporary directories (as currently with `tor_persist`).
//!
//!  * The serde-based `StorageHandle` requires `&mut` for writing.
//!    This ensures proper serialisation of 1. read-modify-write cycles
//!    and 2. use of the temporary file.
//!    Or to put it another way, we model `StorageHandle`
//!    as *containing* a `T` without interior mutability.
//!
//!  * There's a way to get a raw directory for filesystem operations
//!    (currently, will be used for IPT replay logs).
//!
//! ### Implied filesystem structure
//!
//! ```text
//! STATE_DIR/
//! STATE_DIR/KIND/INSTANCE/
//! STATE_DIR/KIND/INSTANCE/lock
//! STATE_DIR/KIND/INSTANCE/SLUG.json
//! STATE_DIR/KIND/INSTANCE/SLUG.new
//! STATE_DIR/KIND/INSTANCE/SLUG/
//!
//! eg
//!
//! STATE_DIR/hss/allium-cepa.lock
//! STATE_DIR/hss/allium-cepa/ipts.json
//! STATE_DIR/hss/allium-cepa/iptpub.json
//! STATE_DIR/hss/allium-cepa/iptreplay/
//! STATE_DIR/hss/allium-cepa/iptreplay/9aa9517e6901c280a550911d3a3c679630403db1c622eedefbdf1715297f795f.bin
//! ```
//!
//! (The lockfile is outside the instance directory to facilitate
//! concurrency-correct deletion.)
//!
//! ### Comprehensive example
//!
//! ```
//! use std::{collections::HashSet, fmt, time::Duration};
//! use tor_error::{into_internal, Bug};
//! use tor_persist::slug::SlugRef;
//! use tor_persist::state_dir;
//! use state_dir::{InstanceIdentity, InstancePurgeHandler};
//! use state_dir::{InstancePurgeInfo, InstanceStateHandle, StateDirectory, StorageHandle};
//! #
//! # // fake up some things; we do this rather than using real ones
//! # // since this example will move, with the module, to a lower level crate.
//! # struct OnionService { }
//! # #[derive(derive_more::Display)] struct HsNickname(String);
//! # type Error = anyhow::Error;
//! # mod ipt_mgr { pub mod persist {
//! #     #[derive(serde::Serialize, serde::Deserialize)] pub struct StateRecord {}
//! # } }
//!
//! impl InstanceIdentity for HsNickname {
//!     fn kind() -> &'static str { "hss" }
//!     fn write_identity(&self, f: &mut fmt::Formatter) -> fmt::Result {
//!         write!(f, "{self}")
//!     }
//! }
//!
//! impl OnionService {
//!     fn new(
//!         nick: HsNickname,
//!         state_dir: &StateDirectory,
//!     ) -> Result<Self, Error> {
//!         let instance_state = state_dir.acquire_instance(&nick)?;
//!         let replay_log_dir = instance_state.raw_subdir("ipt_replay")?;
//!         let ipts_storage: StorageHandle<ipt_mgr::persist::StateRecord> =
//!             instance_state.storage_handle("ipts")?;
//!         // ..
//! #       Ok(OnionService { })
//!     }
//! }
//!
//! struct PurgeHandler<'h>(&'h HashSet<&'h str>, Duration);
//! impl InstancePurgeHandler for PurgeHandler<'_> {
//!     fn name_filter(&mut self, id: &SlugRef) -> state_dir::Result<state_dir::Liveness> {
//!         Ok(if self.0.contains(id.as_str()) {
//!             state_dir::Liveness::Live
//!         } else {
//!             state_dir::Liveness::PossiblyUnused
//!         })
//!     }
//!     fn retain_unused_for(&mut self, id: &SlugRef) -> state_dir::Result<Duration> {
//!         Ok(self.1)
//!     }
//!     fn dispose(&mut self, _info: &InstancePurgeInfo, handle: InstanceStateHandle)
//!                -> state_dir::Result<()> {
//!         // here might be a good place to delete keys too
//!         handle.purge()
//!     }
//! }
//! pub fn expire_hidden_services(
//!     state_dir: &StateDirectory,
//!     currently_configured_nicks: &HashSet<&str>,
//!     retain_for: Duration,
//! ) -> Result<(), Error> {
//!     state_dir.purge_instances(&mut PurgeHandler(currently_configured_nicks, retain_for))?;
//!     Ok(())
//! }
//! ```
//!
//! ### Platforms without a filesystem
//!
//! The implementation and (in places) the documentation
//! is in terms of filesystems.
//! But, everything except `InstanceStateHandle::raw_subdir`
//! is abstract enough to implement some other way.
//!
//! If we wish to support such platforms, the approach is:
//!
//!  * Decide on an approach for `StorageHandle`
//!    and for each caller of `raw_subdir`.
//!
//!  * Figure out how the startup code will look.
//!    (Currently everything is in terms of `fs_mistrust` and filesystems.)
//!
//!  * Provide a version of this module with a compatible API
//!    in terms of whatever underlying facilities are available.
//!    Use `#[cfg]` to select it.
//!    Don't implement `raw_subdir`.
//!
//!  * Call sites using `raw_subdir` will no longer compile.
//!    Use `#[cfg]` at call sites to replace the `raw_subdir`
//!    with whatever is appropriate for the platform.

#![allow(unused_variables, unused_imports, dead_code)] // TODO HSS remove

use std::cell::Cell;
use std::fmt::{self, Display};
use std::fs;
use std::iter;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use derive_more::{AsRef, Deref, Into};
use serde::{de::DeserializeOwned, Serialize};
use void::Void;

use fs_mistrust::{CheckedDir, Mistrust};
use fslock_guard::LockFileGuard;
use tor_error::ErrorReport as _;
use tor_error::{bad_api_usage, into_bad_api_usage, Bug};
use tracing::trace;

use crate::err::{Action, ErrorSource, Resource};
use crate::load_store;
use crate::slug::{self, BadSlug, Slug, SlugRef, TryIntoSlug};
pub use crate::Error;

/// TODO HSS remove
type Todo = Void;

use std::result::Result as StdResult;

use std::path::MAIN_SEPARATOR as PATH_SEPARATOR;

/// [`Result`](StdResult) throwing a [`state_dir::Error`](Error)
pub type Result<T> = StdResult<T, Error>;

/// The whole program's state directory
///
/// Representation of `[storage] state_dir` and `permissions`
/// from the Arti configuration.
///
/// This type does not embody any subpaths relating to
/// any particular facility within Arti.
///
/// Constructing a `StateDirectory` may involve filesystem permissions checks,
/// so ideally it would be created once per process for performance reasons.
///
/// Existence of a `StateDirectory` also does not imply exclusive access.
///
/// This type is passed to each facility's constructor;
/// the facility implements [`InstanceIdentity`]
/// and calls [`acquire_instance`](StateDirectory::acquire_instance).
///
/// ### Use for caches
///
/// In principle this type and the methods and subtypes available
/// would be suitable for cache data as well as state data.
///
/// However the locking scheme does not tolerate random removal of files.
/// And cache directories are sometimes configured to point to locations
/// with OS-supplied automatic file cleaning.
/// That would not be correct,
/// since the automatic file cleaner might remove an in-use lockfile,
/// effectively unlocking the instance state
/// even while a process exists that thinks it still has the lock.
#[derive(Debug)]
pub struct StateDirectory {
    /// The actual directory, including mistrust config
    dir: CheckedDir,
}

/// An instance of a facility that wants to save persistent state (caller-provided impl)
///
/// Each value of a type implementing `InstanceIdentity`
/// designates a specific instance of a specific facility.
///
/// For example, `HsNickname` implements `state_dir::InstanceIdentity`.
///
/// The kind and identity are [`slug`]s.
pub trait InstanceIdentity {
    /// Return the kind.  For example `hss` for a Tor Hidden Service.
    ///
    /// This must return a fixed string,
    /// since usually all instances represented the same Rust type
    /// are also the same kind.
    ///
    /// The returned value must be valid as a [`slug`].
    //
    // This precludes dynamically chosen instance kind identifiers.
    // If we ever want that, we'd need an InstanceKind trait that is implemented
    // not for actual instances, but for values representing a kind.
    fn kind() -> &'static str;

    /// Obtain identity
    ///
    /// The instance identity distinguishes different instances of the same kind.
    ///
    /// For example, for a Tor Hidden Service the identity is the nickname.
    ///
    /// The generated string must be valid as a [`slug`].
    /// If it is not, the functions in this module will throw `Bug` errors.
    /// (Returning `fmt::Error` will cause a panic, as is usual with the fmt API.)
    fn write_identity(&self, f: &mut fmt::Formatter) -> fmt::Result;
}

/// For a facility to be expired using [`purge_instances`](StateDirectory::purge_instances) (caller-provided impl)
///
/// A filter which decides which instances to delete,
/// and deletes them if appropriate.
///
/// See [`purge_instances`](StateDirectory::purge_instances) for full documentation.
pub trait InstancePurgeHandler {
    /// Can we tell by its name that this instance is still live ?
    fn name_filter(&mut self, identity: &SlugRef) -> Result<Liveness>;

    /// How long should we retain an unused instance for ?
    ///
    /// Many implementations won't need to use `identity`.
    /// To pass every possibly-unused instance
    /// through to `dispose`, return `Duration::ZERO`.
    fn retain_unused_for(&mut self, identity: &SlugRef) -> Result<Duration>;

    /// Decide whether to keep this instance
    ///
    /// When it has made its decision, `dispose` should
    /// either call [`delete`](InstanceStateHandle::purge),
    /// or simply drop `handle`.
    ///
    /// Called only after `name_filter` returned [`Liveness::PossiblyUnused`]
    /// and only if the instance has not been acquired or modified recently.
    ///
    /// `info` includes the instance name and other useful information
    /// such as the last modification time.
    fn dispose(&mut self, info: &InstancePurgeInfo, handle: InstanceStateHandle) -> Result<()>;
}

/// Information about an instance, passed to [`InstancePurgeHandler::dispose`]
#[derive(amplify::Getters, AsRef)]
pub struct InstancePurgeInfo<'i> {
    /// The instance's identity string
    #[as_ref]
    identity: &'i SlugRef,

    /// When the instance state was last updated, according to the filesystem timestamps
    ///
    /// See `[InstanceStateHandle::purge_instances]`
    /// for details of what kinds of events count as modifications.
    last_modified: SystemTime,
}

/// Is an instance still relevant?
///
/// Returned by [`InstancePurgeHandler::name_filter`].
///
/// See [`StateDirectory::purge_instances`] for details of the semantics.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[allow(clippy::exhaustive_enums)] // this is a boolean
pub enum Liveness {
    /// This instance is not known to be interesting
    ///
    /// It could be perhaps expired, if it's been long enough
    PossiblyUnused,
    /// This instance is still wanted
    Live,
}

/// Instance identity string formatter, type-erased
type InstanceIdWriter<'i> = &'i dyn Fn(&mut fmt::Formatter) -> fmt::Result;

impl StateDirectory {
    /// Create a new `StateDirectory` from a directory and mistrust configuration
    pub fn new(state_dir: impl AsRef<Path>, mistrust: &Mistrust) -> Result<Self> {
        /// Implementation, taking non-generic path
        fn inner(path: &Path, mistrust: &Mistrust) -> Result<StateDirectory> {
            let resource = || Resource::Directory {
                dir: path.to_owned(),
            };
            let handle_err = |source| Error::new(source, Action::Initializing, resource());

            let dir = mistrust
                .verifier()
                .make_secure_dir(path)
                .map_err(handle_err)?;

            Ok(StateDirectory { dir })
        }
        inner(state_dir.as_ref(), mistrust)
    }

    /// Acquires (creates and locks) a storage for an instance
    ///
    /// Ensures the existence and suitability of a subdirectory named `kind/identity`,
    /// and locks it for exclusive access.
    pub fn acquire_instance<I: InstanceIdentity>(
        &self,
        identity: &I,
    ) -> Result<InstanceStateHandle> {
        /// Implementation, taking non-generic values for identity
        fn inner(
            sd: &StateDirectory,
            kind_str: &'static str,
            id_writer: InstanceIdWriter,
        ) -> Result<InstanceStateHandle> {
            sd.with_instance_path_pieces(kind_str, id_writer, |kind, id, resource| {
                let handle_err =
                    |action, source: ErrorSource| Error::new(source, action, resource());

                // Obtain (creating if necessary) a subdir for a Checked
                let make_secure_directory = |parent: &CheckedDir, subdir| {
                    let resource = || Resource::Directory {
                        dir: parent.as_path().join(subdir),
                    };
                    parent
                        .make_secure_directory(subdir)
                        .map_err(|source| Error::new(source, Action::Initializing, resource()))
                };

                // ---- obtain the lock ----

                let kind_dir = make_secure_directory(&sd.dir, kind)?;

                let lock_path = kind_dir
                    .join(format!("{id}.lock"))
                    .map_err(|source| handle_err(Action::Initializing, source.into()))?;

                let flock_guard = match LockFileGuard::try_lock(&lock_path) {
                    Ok(Some(y)) => {
                        trace!("locked {lock_path:?}");
                        y.into()
                    }
                    Err(source) => {
                        trace!("locking {lock_path:?}, error {}", source.report());
                        return Err(handle_err(Action::Locking, source.into()));
                    }
                    Ok(None) => {
                        trace!("locking {lock_path:?}, in use",);
                        return Err(handle_err(Action::Locking, ErrorSource::AlreadyLocked));
                    }
                };

                // ---- we have the lock, calculate the directory (creating it if need be) ----

                let dir = make_secure_directory(&kind_dir, id)?;

                Ok(InstanceStateHandle { dir, flock_guard })
            })
        }

        inner(self, I::kind(), &|f| identity.write_identity(f))
    }

    /// Given a kind and id, obtain pieces of its path and call a "doing work" callback
    ///
    /// This function factors out common functionality needed by
    /// [`StateDirectory::acquire_instance`] and [StateDirectory::instance_peek_storage`],
    /// particularly relating to instance kind and id, and errors.
    ///
    /// `kind` and `id` are from an `InstanceIdentity`.
    fn with_instance_path_pieces<T>(
        self: &StateDirectory,
        kind_str: &'static str,
        id_writer: InstanceIdWriter,
        call: impl FnOnce(&SlugRef, &SlugRef, &dyn Fn() -> Resource) -> Result<T>,
    ) -> Result<T> {
        /// Struct that impls `Display` for formatting an instance id
        //
        // This exists because we want implementors of InstanceIdentity to be able to
        // use write! to format their identity string.
        struct InstanceIdDisplay<'i>(InstanceIdWriter<'i>);

        impl Display for InstanceIdDisplay<'_> {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                (self.0)(f)
            }
        }
        let id_string = InstanceIdDisplay(id_writer).to_string();

        // Both we and caller use this for our error reporting
        let resource = || Resource::InstanceState {
            state_dir: self.dir.as_path().to_owned(),
            kind: kind_str.to_string(),
            identity: id_string.clone(),
        };

        let handle_bad_slug = |source| Error::new(source, Action::Initializing, resource());

        if kind_str.is_empty() {
            return Err(handle_bad_slug(BadSlug::EmptySlugNotAllowed));
        }
        let kind = SlugRef::new(kind_str).map_err(handle_bad_slug)?;
        let id = SlugRef::new(&id_string).map_err(handle_bad_slug)?;

        call(kind, id, &resource)
    }

    /// List the instances of a particular kind
    ///
    /// Returns the instance identities.
    ///
    /// (The implementation lists subdirectories named `kind_*`.)
    ///
    /// Concurrency:
    /// An instance which is not being removed or created will be
    /// listed (or not) according to whether it's present.
    /// But, in the presence of concurrent calls to `acquire_instance` and `delete`
    /// on different instances,
    /// is not guaranteed to provide a snapshot:
    /// serialisation is not guaranteed across different instances.
    #[allow(clippy::extra_unused_type_parameters)] // TODO HSS remove if possible
    #[allow(unreachable_code)] // TODO HSS remove
    pub fn list_instances<I: InstanceIdentity>(&self) -> impl Iterator<Item = Result<Slug>> {
        todo!();
        iter::empty()
    }

    /// Delete instances according to selections made by the caller
    ///
    /// Each instance is considered in three stages.
    ///
    /// Firstly, it is passed to [`name_filter`](InstancePurgeHandler::name_filter).
    /// If `name_filter` returns `Live`,
    /// further consideration is skipped and the instance is retained.
    ///
    /// Secondly, the last time the instance was written to is calculated,
    // This must be done with the lock held, for correctness
    // but the lock must be acquired in a way that doesn't itself update the modification time.
    // On Unix this is straightforward because opening for write doesn't update the mtime.
    // If this is hard on another platform, we'll need a separate stamp file updated
    // by an explicit Acquire operation.
    // We should have a test to check that this all works as expected.
    /// and compared to the return value from
    /// [`retain_unused_for`](InstancePurgeHandler::retain_unused_for).
    /// Again, this might mean ensure the instance is retained.
    ///
    /// Thirdly, the resulting `InstanceStateHandle` is passed to
    /// [`dispose`](InstancePurgeHandler::dispose).
    /// `dispose` may choose to call `handle.delete()`,
    /// or simply drop the handle.
    ///
    /// Concurrency:
    /// In the presence of multiple concurrent calls to `acquire_instance` and `delete`:
    /// `filter` may be called for an instance which is being created or deleted
    /// by another task.
    /// `dispose` will be properly serialised with other activities on the same instance,
    /// as implied by it receiving an `InstanceStateHandle`.
    ///
    /// Instances which have been acquired
    /// or modified more recently than `retain_unused_for`
    /// will not be offered to `dispose`.
    ///
    /// The expiry time is reset by calls to `acquire_instance`,
    /// `StorageHandle::store` and `InstanceStateHandle::raw_subdir`;
    /// it *may* be reset by calls to `StorageHandle::delete`.
    pub fn purge_instances<I: InstancePurgeHandler>(&self, filter: &mut I) -> Result<()> {
        todo!()
    }

    /// Tries to peek at something written by `StorageHandle::store`
    ///
    /// It is guaranteed that this will return either the `T` that was stored,
    /// or `None` if `store` was never called,
    /// or `StorageHandle::delete` was called
    ///
    /// So the operation is atomic, but there is no further synchronisation.
    //
    // Not sure if we need this, but it's logically permissible
    pub fn instance_peek_storage<I: InstanceIdentity, T: DeserializeOwned>(
        &self,
        identity: &I,
        slug: &(impl TryIntoSlug + ?Sized),
    ) -> Result<Option<T>> {
        self.with_instance_path_pieces(
            I::kind(),
            &|f| identity.write_identity(f),
            // This closure is generic over T, so with_instance_path_pieces will be too;
            // this isn't desirable (code bloat) but avoiding it would involves some contortions.
            |kind_slug: &SlugRef, id_slug: &SlugRef, _resource| {
                // Throwing this error here will give a slightly wrong Error for this Bug
                // (because with_instance_path_pieces has its own notion of Action & Resource)
                // but that seems OK.
                let storage_slug = slug.try_into_slug()?;

                let rel_fname = format!(
                    "{}{PATH_SEPARATOR}{}{PATH_SEPARATOR}{}.json",
                    kind_slug, id_slug, storage_slug,
                );

                let target = load_store::Target {
                    dir: &self.dir,
                    rel_fname: rel_fname.as_ref(),
                };

                target
                    .load()
                    // This Resource::File isn't consistent with those from StorageHandle:
                    // StorageHandle's `container` is the instance directory;
                    // here `container` is the top-level `state_dir`,
                    // and `file` is `KIND+INSTANCE/STORAGE.json".
                    .map_err(|source| {
                        Error::new(
                            source,
                            Action::Loading,
                            Resource::File {
                                container: self.dir.as_path().to_owned(),
                                file: rel_fname.into(),
                            },
                        )
                    })
            },
        )
    }
}

/// State or cache directory for an instance of a facility
///
/// Implies exclusive access:
/// there is only one `InstanceStateHandle` at a time,
/// across any number of processes, tasks, and threads,
/// for the same instance.
///
/// But this type is `Clone` and the exclusive access is shared across all clones.
/// Users of the `InstanceStateHandle` must ensure that functions like
/// `storage_handle` and `raw_directory` are only called once with each `slug`.
/// (Typically, the slug is fixed, so this is straightforward.)
///
/// # Slug uniqueness and syntactic restrictions
///
/// Methods on `InstanceStateHandle` typically take a [`TryIntoSlug`].
///
/// **It is important that slugs are distinct within an instance.**
///
/// Specifically:
/// each slug provided to a method on the same [`InstanceStateHandle`]
/// (or a clone of it)
/// must be different.
/// Violating this rule does not result in memory-unsafety,
/// but might result in incorrect operation due to concurrent filesystem access,
/// including possible data loss and corruption.
/// (Typically, the slug is fixed, and the [`StorageHandle`]s are usually
/// obtained during instance construction, so ensuring this is straightforward.)
///
/// There are also syntactic restrictions on slugs.  See [slug].
// We could implement a runtime check for this by retaining a table of in-use slugs,
// possibly only with `cfg(debug_assertions)`.  However I think this isn't worth the code:
// it would involve an Arc<Mutex<SlugsInUseTable>> in InstanceStateHnndle and StorageHandle,
// and Drop impls to remove unused entries (and `raw_subdir` would have imprecise checking
// unless it returned a Drop newtype around CheckedDir).
#[derive(Debug)]
pub struct InstanceStateHandle {
    /// The directory
    dir: CheckedDir,
    /// Lock guard
    flock_guard: Arc<LockFileGuard>,
}

impl InstanceStateHandle {
    /// Obtain a [`StorageHandle`], usable for storing/retrieving a `T`
    ///
    /// [`slug` has syntactic and uniqueness restrictions.](InstanceStateHandle#slug-uniqueness-and-syntactic-restrictions)
    pub fn storage_handle<T>(
        &self,
        slug: &(impl TryIntoSlug + ?Sized),
    ) -> Result<StorageHandle<T>> {
        /// Implementation, not generic over `slug` and `T`
        fn inner(
            ih: &InstanceStateHandle,
            slug: StdResult<Slug, BadSlug>,
        ) -> Result<(CheckedDir, String, Arc<LockFileGuard>)> {
            let slug = slug?;
            let instance_dir = ih.dir.clone();
            let leafname = format!("{slug}.json");
            let flock_guard = ih.flock_guard.clone();
            Ok((instance_dir, leafname, flock_guard))
        }

        let (instance_dir, leafname, flock_guard) = inner(self, slug.try_into_slug())?;
        Ok(StorageHandle {
            instance_dir,
            leafname,
            marker: PhantomData,
            flock_guard,
        })
    }

    /// Obtain a raw filesystem subdirectory, within the directory for this instance
    ///
    /// This API is unsuitable platforms without a filesystem accessible via `std::fs`.
    /// May therefore only be used within Arti for features
    /// where we're happy to not to support such platforms (eg WASM without WASI)
    /// without substantial further work.
    ///
    /// [`slug` has syntactic and uniqueness restrictions.](InstanceStateHandle#slug-uniqueness-and-syntactic-restrictions)
    pub fn raw_subdir(&self, slug: &(impl TryIntoSlug + ?Sized)) -> Result<InstanceRawSubdir> {
        /// Implementation, not generic over `slug`
        fn inner(
            ih: &InstanceStateHandle,
            slug: StdResult<Slug, BadSlug>,
        ) -> Result<InstanceRawSubdir> {
            let slug = slug?;
            (|| {
                trace!("ensuring/using {:?}/{:?}", ih.dir.as_path(), slug.as_str());
                let dir = ih.dir.make_secure_directory(&slug)?;
                let flock_guard = ih.flock_guard.clone();
                Ok::<_, ErrorSource>(InstanceRawSubdir { dir, flock_guard })
            })()
            .map_err(|source| {
                Error::new(
                    source,
                    Action::Initializing,
                    Resource::Directory {
                        dir: ih.dir.as_path().join(slug),
                    },
                )
            })
        }
        inner(self, slug.try_into_slug())
    }

    /// Unconditionally delete this instance directory
    ///
    /// For expiry, use `StateDirectory::purge_instances`,
    /// and then call this in the `dispose` method.
    ///
    /// Will return a `BadAPIUsage` if other clones of this `InstanceStateHandle` exist.
    pub fn purge(self) -> Result<()> {
        let dir = self.dir.as_path();

        (|| {
            // use Arc::into_inner on the lock object,
            // to make sure we're actually the only surviving InstanceStateHandle
            let flock_guard = Arc::into_inner(self.flock_guard).ok_or_else(|| {
                bad_api_usage!(
 "InstanceStateHandle::purge called for {:?}, but other clones of the handle exist",
                    self.dir.as_path(),
                )
            })?;

            trace!("purging {:?} (and .lock)", dir);
            fs::remove_dir_all(dir)?;
            flock_guard.delete_lock_file(
                // dir.with_extension is right because the last component of dir
                // is KIND+ID which doesn't contain `.` so no extension will be stripped
                dir.with_extension("lock"),
            )?;

            Ok::<_, ErrorSource>(())
        })()
        .map_err(|source| {
            Error::new(
                source,
                Action::Deleting,
                Resource::Directory { dir: dir.into() },
            )
        })
    }
}

/// A place in the state or cache directory, where we can load/store a serialisable type
///
/// Implies exclusive access.
///
/// Rust mutability-xor-sharing rules enforce proper synchronisation,
/// unless multiple `StorageHandle`s are created
/// using the same [`InstanceStateHandle`] and slug.
pub struct StorageHandle<T> {
    /// The directory and leafname
    instance_dir: CheckedDir,
    /// `SLUG.json`
    leafname: String,
    /// We're not sync, and we can load and store a `T`
    marker: PhantomData<Cell<T>>,
    /// Clone of the InstanceStateHandle's lock
    flock_guard: Arc<LockFileGuard>,
}

// Like tor_persist, but writing needs `&mut`
impl<T: Serialize + DeserializeOwned> StorageHandle<T> {
    /// Load this persistent state
    ///
    /// `None` means the state was most recently [`delete`](StorageHandle::delete)ed
    pub fn load(&self) -> Result<Option<T>> {
        self.with_load_store_target(Action::Loading, |t| t.load())
    }
    /// Store this persistent state
    pub fn store(&mut self, v: &T) -> Result<()> {
        self.with_load_store_target(Action::Storing, |t| t.store(v))
    }
    /// Delete this persistent state
    pub fn delete(&mut self) -> Result<()> {
        self.with_load_store_target(Action::Deleting, |t| t.delete())
    }

    /// Operate using a `load_store::Target`
    fn with_load_store_target<R, F>(&self, action: Action, f: F) -> Result<R>
    where
        F: FnOnce(load_store::Target<'_>) -> std::result::Result<R, ErrorSource>,
    {
        f(load_store::Target {
            dir: &self.instance_dir,
            rel_fname: self.leafname.as_ref(),
        })
        .map_err(self.map_err(action))
    }

    /// Helper to convert an `ErrorSource` to an `Error`, if we were performing `action`
    fn map_err(&self, action: Action) -> impl FnOnce(ErrorSource) -> Error {
        let resource = self.err_resource();
        move |source| crate::Error::new(source, action, resource)
    }

    /// Return the proper `Resource` for reporting errors
    fn err_resource(&self) -> Resource {
        Resource::File {
            // TODO ideally we would remember what proportion of instance_dir
            // came from the original state_dir, so we can put state_dir in the container
            container: self.instance_dir.as_path().to_owned(),
            file: self.leafname.clone().into(),
        }
    }
}

/// Subdirectory within an instance's state, for raw filesystem operations
///
/// Dereferences to `fs_mistrust::CheckedDir` and can be used mostly like one.
/// Obtained from [`InstanceStateHandle::raw_subdir`].
///
/// Existence of this value implies exclusive access to the instance.
#[derive(Deref, Clone)]
pub struct InstanceRawSubdir {
    /// The actual directory, as a [`fs_mistrust::CheckedDir`]
    #[deref]
    dir: CheckedDir,
    /// Clone of the InstanceStateHandle's lock
    flock_guard: Arc<LockFileGuard>,
}

#[cfg(test)]
mod test {
    // @@ begin test lint list maintained by maint/add_warning @@
    #![allow(clippy::bool_assert_comparison)]
    #![allow(clippy::clone_on_copy)]
    #![allow(clippy::dbg_macro)]
    #![allow(clippy::print_stderr)]
    #![allow(clippy::print_stdout)]
    #![allow(clippy::single_char_pattern)]
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::unchecked_duration_subtraction)]
    #![allow(clippy::useless_vec)]
    #![allow(clippy::needless_pass_by_value)]
    //! <!-- @@ end test lint list maintained by maint/add_warning @@ -->

    use super::*;
    use serde::{Deserialize, Serialize};
    use std::fmt::Display;
    use std::io;
    use test_temp_dir::test_temp_dir;
    use tor_error::{into_internal, HasKind as _};
    use tracing_test::traced_test;

    use tor_error::ErrorKind as TEK;

    struct Garlic(Slug);

    impl InstanceIdentity for Garlic {
        fn kind() -> &'static str {
            "garlic"
        }
        fn write_identity(&self, f: &mut fmt::Formatter) -> fmt::Result {
            Display::fmt(&self.0, f)
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
    struct StoredData {
        some_value: i32,
    }

    #[test]
    #[traced_test]
    fn test_api() {
        test_temp_dir!().used_by(|dir| {
            let sd = StateDirectory::new(
                dir,
                &fs_mistrust::Mistrust::new_dangerously_trust_everyone(),
            )
            .unwrap();

            let garlic = Garlic("wild".try_into_slug().unwrap());

            let acquire_instance = || sd.acquire_instance(&garlic);

            let ih = acquire_instance().unwrap();
            let inst_path = dir.join("garlic/wild");
            assert!(fs::metadata(&inst_path).unwrap().is_dir());

            assert_eq!(
                acquire_instance().unwrap_err().kind(),
                TEK::LocalResourceAlreadyInUse,
            );

            let irsd = ih.raw_subdir("raw").unwrap();
            assert!(fs::metadata(irsd.as_path()).unwrap().is_dir());
            assert_eq!(irsd.as_path(), dir.join("garlic").join("wild").join("raw"));

            let mut sh = ih.storage_handle::<StoredData>("stored_data").unwrap();
            let storage_path = dir.join("garlic/wild/stored_data.json");

            let peek = || sd.instance_peek_storage(&garlic, "stored_data");

            let expect_load = |sh: &StorageHandle<_>, expect| {
                let check_loaded = |what, loaded: Result<Option<StoredData>>| {
                    assert_eq!(loaded.unwrap().as_ref(), expect, "{what}");
                };
                check_loaded("load", sh.load());
                check_loaded("peek", peek());
            };

            expect_load(&sh, None);

            let to_store = StoredData { some_value: 42 };
            sh.store(&to_store).unwrap();
            assert!(fs::metadata(storage_path).unwrap().is_file());

            expect_load(&sh, Some(&to_store));

            sh.delete().unwrap();

            expect_load(&sh, None);

            drop(sh);
            drop(irsd);
            ih.purge().unwrap();

            assert_eq!(peek().unwrap(), None);
            assert_eq!(
                fs::metadata(&inst_path).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
        });
    }
}