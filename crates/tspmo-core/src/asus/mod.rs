//! ASUS laptop hardware: fans, power limits, battery, firmware toggles, lighting, lid displays,
//! hotkeys. Pure logic and sysfs access through [`tspmo_sys::SysRoot`]; the daemon (`tspmod`)
//! owns state, D-Bus and threads. Capabilities are discovered, never assumed.

pub mod battery;
pub mod fans;
pub mod hardware;
pub mod hid;
pub mod limits;
pub mod quirks;

/// Writable copy of the GU605MI fixture, for tests here and in `tspmod`.
#[doc(hidden)]
pub mod testfix;
