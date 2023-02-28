#![cfg_attr(docsrs, feature(doc_auto_cfg, doc_cfg))]
#![doc = include_str!("../README.md")]
// @@ begin lint list maintained by maint/add_warning @@
#![cfg_attr(not(ci_arti_stable), allow(renamed_and_removed_lints))]
#![cfg_attr(not(ci_arti_nightly), allow(unknown_lints))]
#![deny(missing_docs)]
#![warn(noop_method_call)]
#![deny(unreachable_pub)]
#![warn(clippy::all)]
#![deny(clippy::await_holding_lock)]
#![deny(clippy::cargo_common_metadata)]
#![deny(clippy::cast_lossless)]
#![deny(clippy::checked_conversions)]
#![warn(clippy::cognitive_complexity)]
#![deny(clippy::debug_assert_with_mut_call)]
#![deny(clippy::exhaustive_enums)]
#![deny(clippy::exhaustive_structs)]
#![deny(clippy::expl_impl_clone_on_copy)]
#![deny(clippy::fallible_impl_from)]
#![deny(clippy::implicit_clone)]
#![deny(clippy::large_stack_arrays)]
#![warn(clippy::manual_ok_or)]
#![deny(clippy::missing_docs_in_private_items)]
#![deny(clippy::missing_panics_doc)]
#![warn(clippy::needless_borrow)]
#![warn(clippy::needless_pass_by_value)]
#![warn(clippy::option_option)]
#![warn(clippy::rc_buffer)]
#![deny(clippy::ref_option_ref)]
#![warn(clippy::semicolon_if_nothing_returned)]
#![warn(clippy::trait_duplication_in_bounds)]
#![deny(clippy::unnecessary_wraps)]
#![warn(clippy::unseparated_literal_suffix)]
#![deny(clippy::unwrap_used)]
#![allow(clippy::let_unit_value)] // This can reasonably be done for explicitness
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::significant_drop_in_scrutinee)] // arti/-/merge_requests/588/#note_2812945
#![allow(clippy::result_large_err)] // temporary workaround for arti#587
//! <!-- @@ end lint list maintained by maint/add_warning @@ -->

mod connect;
mod err;
mod keys;
mod state;

use std::future::Future;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use educe::Educe;

use tor_circmgr::isolation::Isolation;
use tor_circmgr::{CircMgr, OnionConnectError, OnionServiceConnector};
use tor_hscrypto::pk::HsId;
use tor_netdir::NetDirProvider;
use tor_proto::circuit::ClientCirc;
use tor_rtcompat::Runtime;

pub use err::{HsClientConnError, StartupError};
pub use keys::{HsClientSecretKeys, HsClientSecretKeysBuilder};

use state::Services;

/// An object that negotiates connections with onion services
#[derive(Educe)]
#[educe(Clone)]
pub struct HsClientConnector<R: Runtime, D: state::MockableConnectorData = connect::Data> {
    /// The runtime
    runtime: R,
    /// A [`CircMgr`] that we use to build circuits to HsDirs, introduction
    /// points, and rendezvous points.
    //
    // TODO hs: currently this is a circular set of Arc, since the CircMgr will
    // have to hold an Arc<OnionServiceConnector>.  We should make one Weak.
    // A. We should probably abolish this instead, see comments for OnionServiceConnector -Diziet
    //
    // TODO hs: Maybe we can make a trait that only gives a minimal "build a
    // circuit" API from CircMgr, so that we can have this be a dyn reference
    // too?
    circmgr: Arc<CircMgr<R>>,
    /// A [`NetDirProvider`] that we use to pick rendezvous points.
    //
    // TODO hs: Should this be weak too?   A. No, it's a downward reference. -Diziet
    netdir_provider: Arc<dyn NetDirProvider>,
    /// Information we are remembering about different onion services.
    //
    // TODO hs: if we implement cache isolation or state isolation, we might
    // need multiple instances of this.
    services: Arc<Mutex<state::Services<D>>>,
    /// For mocking in tests of `state.rs`
    mock_for_state: D::MockGlobalState,
}

impl<R: Runtime> HsClientConnector<R, connect::Data> {
    /// Create a new `HsClientConnector`
    pub fn new(
        runtime: R,
        circmgr: Arc<CircMgr<R>>,
        netdir_provider: Arc<dyn NetDirProvider>,
        // TODO HS: there should be a config here, we will probably need it at some point
        // TODO HS: needs a parameter which lets us periodically expire old HS data/circuits
    ) -> Result<Self, StartupError> {
        Ok(HsClientConnector {
            runtime,
            circmgr,
            netdir_provider,
            services: Arc::new(Mutex::new(Services::default())),
            mock_for_state: (),
        })
    }

    /// Connect to a hidden service
    ///
    /// Each HS connection request must provide the appropriate
    /// client authentication keys to use -
    /// or [`default`](HsClientSecretKeys::default) if client auth is not required.
    //
    // This returns an explicit `impl Future` so that we can write the `Send` bound.
    // Without this, it is possible for `Services::get_or_launch_connection`
    // to not return a `Send` future.
    // https://gitlab.torproject.org/tpo/core/arti/-/merge_requests/1034#note_2881718
    pub fn get_or_launch_connection(
        &self,
        hs_id: HsId,
        secret_keys: HsClientSecretKeys,
        isolation: Box<dyn Isolation>,
    ) -> impl Future<Output = Result<ClientCirc, HsClientConnError>> + Send + Sync + '_ {
        Services::get_or_launch_connection(self, hs_id, isolation, secret_keys)
    }
}

#[async_trait]
impl<R: Runtime> OnionServiceConnector for HsClientConnector<R> {
    #[allow(dead_code, unused_variables)] // TODO hs implement this function or remove this trait
    async fn create_connection(&self, service_id: HsId) -> Result<ClientCirc, OnionConnectError> {
        todo!() // TODO hs
    }
}
