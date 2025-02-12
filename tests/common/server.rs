use async_trait::async_trait;

#[async_trait]
pub trait Server {
    async fn stop(self: Box<Self>) -> usize;
    // This function `address` is only used by crate `multiple_upstream_tests`.
    // Crate `single_upstream_tests` use `Server` but don't use the function, so the compiler
    // has a warn about unused import.
    // We add the macro to eliminate the warn.
    #[allow(dead_code)]
    fn address(&self) -> String;
}
