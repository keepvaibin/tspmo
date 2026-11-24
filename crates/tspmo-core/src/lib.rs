//! tspmo core: what this machine can do ([`machine`]), what is wrong with it ([`doctor`]),
//! how each power profile tunes it ([`power`]) and its GPU modes ([`gpu`]).

pub mod display;
pub mod doctor;
pub mod gpu;
pub mod machine;
pub mod power;
pub mod topology;
pub mod usbpm;

pub use tspmo_sys::SysRoot;
