mod protocol;
mod runtime;
mod worker;
mod worker_engine;

pub(super) fn run_worker_from_args() -> Result<(), ()> {
    worker::run_from_args()
}
