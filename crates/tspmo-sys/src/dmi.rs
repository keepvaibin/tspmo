//! Machine identity from `/sys/class/dmi/id`. Serial numbers and UUIDs are never read.

use crate::SysRoot;

const DIR: &str = "/sys/class/dmi/id";

/// Fields that identify a model. Safe to share (no serials, no UUIDs).
pub const PUBLIC_FIELDS: &[&str] = &[
    "sys_vendor",
    "product_name",
    "product_family",
    "product_version",
    "product_sku",
    "board_vendor",
    "board_name",
    "board_version",
    "bios_vendor",
    "bios_version",
    "bios_date",
    "chassis_type",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dmi {
    pub sys_vendor: String,
    pub product_name: String,
    pub product_family: String,
    pub product_version: String,
    pub board_name: String,
    pub bios_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Asus,
    Lenovo,
    Other,
}

impl Dmi {
    pub fn read(root: &SysRoot) -> Self {
        let get = |f: &str| root.read_opt(format!("{DIR}/{f}")).unwrap_or_default();
        Self {
            sys_vendor: get("sys_vendor"),
            product_name: get("product_name"),
            product_family: get("product_family"),
            product_version: get("product_version"),
            board_name: get("board_name"),
            bios_version: get("bios_version"),
        }
    }

    pub fn vendor(&self) -> Vendor {
        let v = self.sys_vendor.to_ascii_lowercase();
        if v.contains("asus") {
            Vendor::Asus
        } else if v.contains("lenovo") {
            Vendor::Lenovo
        } else {
            Vendor::Other
        }
    }

    /// Human-readable model name. Lenovo keeps the marketing name in `product_version`
    /// (`product_name` is a machine-type code like `82RG`).
    pub fn model(&self) -> &str {
        match self.vendor() {
            Vendor::Lenovo if !self.product_version.is_empty() => &self.product_version,
            _ if !self.product_name.is_empty() => &self.product_name,
            _ => &self.board_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::FakeRoot;

    #[test]
    fn asus_and_lenovo_names() {
        let f = FakeRoot::new("dmi");
        f.file("/sys/class/dmi/id/sys_vendor", "ASUSTeK COMPUTER INC.\n")
            .file(
                "/sys/class/dmi/id/product_name",
                "ROG Zephyrus G16 GU605MI_GU605MI\n",
            )
            .file("/sys/class/dmi/id/board_name", "GU605MI\n");
        let d = Dmi::read(&f.root());
        assert_eq!(d.vendor(), Vendor::Asus);
        assert_eq!(d.model(), "ROG Zephyrus G16 GU605MI_GU605MI");

        let f = FakeRoot::new("dmi-lenovo");
        f.file("/sys/class/dmi/id/sys_vendor", "LENOVO\n")
            .file("/sys/class/dmi/id/product_name", "83DG\n")
            .file("/sys/class/dmi/id/product_version", "Legion Pro 7 16ARX8H\n");
        let d = Dmi::read(&f.root());
        assert_eq!(d.vendor(), Vendor::Lenovo);
        assert_eq!(d.model(), "Legion Pro 7 16ARX8H");
    }
}
