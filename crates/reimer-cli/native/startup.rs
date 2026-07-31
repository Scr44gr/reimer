//! Native startup shim linked with one generated Reimer object.

unsafe extern "C" {
    fn program_main() -> i32;
}

fn main() {
    let session = reimer_runtime::ExecutionSession::begin();
    // SAFETY: The compiler emits `program_main` with this exact signature.
    let exit_code = unsafe { program_main() };
    reimer_runtime::shutdown_job_pools(session.id());
    reimer_runtime::join_session_threads(session.id());
    std::process::exit(exit_code);
}
