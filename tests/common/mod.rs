mod echo_server;
mod error_server;
mod loadbalancer;
mod server;

use std::sync;

pub use echo_server::EchoServer;
// `ErrorServer`` is only used by crate `multiple_upstream_tests`.
// crate `single_upstream_tests` use `common` but don't use `ErrorServer`, so the compiler
// has a warn about unused import.
// We add the macro to eliminate the warn.
#[allow(unused_imports)]
pub use error_server::ErrorServer;
pub use loadbalancer::LoadBalancer;
pub use server::Server;

static INIT_TESTS: sync::Once = sync::Once::new();

pub fn init_logging() {
    INIT_TESTS.call_once(|| {
        pretty_env_logger::formatted_builder()
            .is_test(true)
            .parse_filters("info")
            .init();
    });
}
