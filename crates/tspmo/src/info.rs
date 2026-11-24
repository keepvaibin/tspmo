use tspmo_core::machine::Machine;
use tspmo_sys::fwattr::Kind;

pub fn print(m: &Machine) {
    println!(
        "\x1b[1m{}\x1b[0m  ({}, BIOS {})",
        m.dmi.model(),
        m.dmi.sys_vendor,
        m.dmi.bios_version
    );
    println!(
        "  platform drivers: {}",
        list_or_none(m.platform_drivers.iter().copied())
    );

    match &m.platform_profile {
        Some(p) => println!(
            "  performance mode: {p}  (choices: {})",
            m.platform_profile_choices.join(", ")
        ),
        None => println!("  performance mode: not exposed"),
    }

    match m.on_ac {
        Some(true) => println!("  power source:     AC"),
        Some(false) => println!("  power source:     battery"),
        None => {}
    }
    for b in &m.batteries {
        let mut line = format!("  {}: {} {}%", b.name, b.status, b.capacity.unwrap_or(0));
        if let Some(w) = b.power_w {
            line += &format!(", {w:.1} W");
        }
        if let Some(end) = b.charge_end {
            line += &format!(", charge limit {end}%");
        }
        if let Some(h) = b.health_pct {
            line += &format!(", health {h:.0}%");
        }
        if let Some(c) = b.cycle_count.filter(|c| *c > 0) {
            line += &format!(", {c} cycles");
        }
        println!("{line}");
    }

    for g in &m.gpus {
        println!(
            "  GPU {} {} [{:04x}]: driver {}, {}{}",
            g.address,
            g.vendor_name(),
            g.device,
            g.driver.as_deref().unwrap_or("none"),
            g.runtime_status.as_deref().unwrap_or("?"),
            g.power_state
                .as_deref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default(),
        );
    }

    for b in &m.backlights {
        if !b.sampled {
            // NVIDIA's backlight: reading it would wake the GPU.
            println!("  backlight {} ({}): not read (NVIDIA)", b.name, b.kind);
            continue;
        }
        println!(
            "  backlight {} ({}): {}/{} ({:.1}%)",
            b.name,
            b.kind,
            b.brightness,
            b.max,
            b.percent()
        );
    }

    if !m.hwmons.is_empty() {
        println!(
            "  sensors/fans:     {}",
            m.hwmons
                .iter()
                .map(|h| h.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    for d in &m.fw_drivers {
        println!("  firmware attributes ({d}):");
        for a in m.fw_attributes.iter().filter(|a| &a.driver == d) {
            let range = match &a.kind {
                Kind::Integer { min, max, .. } => format!("{min}..{max}"),
                Kind::Enumeration { values } => values.join("|"),
                Kind::String => "text".into(),
                Kind::Other(t) => t.clone(),
            };
            println!(
                "    {:<22} = {:<8} [{range}]",
                a.name,
                a.current.as_deref().unwrap_or("?")
            );
        }
    }
}

fn list_or_none<'a>(it: impl Iterator<Item = &'a str>) -> String {
    let v: Vec<&str> = it.collect();
    if v.is_empty() { "none".into() } else { v.join(", ") }
}
