//! Optional logging shims. Active only with the `logging` Cargo feature.

macro_rules! log_error {
    ($($arg:tt)*) => {{
        #[cfg(feature = "logging")]
        ::log::error!($($arg)*);
    }};
}

macro_rules! log_info {
    ($($arg:tt)*) => {{
        #[cfg(feature = "logging")]
        ::log::info!($($arg)*);
    }};
}
