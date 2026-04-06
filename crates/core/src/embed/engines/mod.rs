#[cfg(feature = "candle")]
pub mod candle;
pub mod dispatch;
#[cfg(feature = "fastembed")]
pub mod fastembed;
pub mod sbert;
pub mod aux_config;
