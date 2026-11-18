//! Backend real no Windows. Nos outros sistemas o binário de demonstração
//! usa [`crate::fake::FakeBackend`].

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub use windows::WindowsBackend;
