mod axdl;
mod peer_id;
mod protocol;
mod runtime;
mod worker;
mod worker_engine;

use crate::xunlei_native::engine::{NativeEngineError, NativeEngineSession};
pub(super) use worker_engine::LinuxWorkerEngine;

pub(super) fn run_worker_from_args() -> Result<(), ()> {
    worker::run_from_args()
}

pub(super) fn start_engine(
    session: NativeEngineSession,
) -> Result<LinuxWorkerEngine, NativeEngineError> {
    let runtime = runtime::discover_best_runtime().ok_or(NativeEngineError::Unavailable)?;
    if !runtime.supports_axdl_bridge() {
        return Err(NativeEngineError::Incompatible);
    }
    worker_engine::LinuxWorkerEngine::start(runtime, session)
}
