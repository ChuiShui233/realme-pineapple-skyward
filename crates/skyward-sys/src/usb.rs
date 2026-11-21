//! The functions the USB port offers, which init sets up as
//! `sys.usb.config` asks: adb while the recovery waits, with files over MTP
//! beside it while file transfer is on.

use crate::{kmsg, prop, set_prop, wait_prop};
use anyhow::{anyhow, ensure, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static MTP: AtomicBool = AtomicBool::new(false);

/// File transfer and adb together, the configuration this build's own init
/// rules (mtp.rc) serve beside init's stock ones.
pub const MTP_ADB: &str = "mtp,adb";

/// What the port offers while nothing else has it.
pub fn resting() -> &'static str {
    if MTP.load(Ordering::Relaxed) {
        MTP_ADB
    } else {
        "adb"
    }
}

/// Whether MTP goes beside adb while the port rests: from when its function
/// has its descriptors until it goes.
pub fn set_mtp(on: bool) {
    MTP.store(on, Ordering::Relaxed);
}

/// Whether init's rules can hand the port to `config` and back. The stock
/// functions of a recovery — adb, sideload and fastboot — always have them,
/// since init's own rules take the port through none for those. This
/// build's file transfer (`mtp,adb`) leans on the phone's init rules
/// (mtp.rc): they start adbd once the port asks for it, and bind the two
/// functions once both have their descriptors. A recovery whose init has
/// no function for MTP, or none for adb, has no way to take the port, so
/// the port is left alone.
pub fn can_switch(config: &str) -> bool {
    match config {
        "adb" | "fastboot" | "sideload" => true,
        MTP_ADB => {
            prop("sys.usb.configfs").as_deref() == Some("1")
                && Path::new("/config/usb_gadget/g1/functions/ffs.mtp").exists()
        }
        _ => false,
    }
}

/// Ask init for `config` and wait until it is in place. Going to "none"
/// also waits for adbd to be gone, so its function is free.
pub fn set(config: &str, timeout: Duration) -> Result<()> {
    set_prop("sys.usb.config", config)?;
    wait_prop("sys.usb.state", config, timeout)?;
    if config == "none" {
        let start = Instant::now();
        while !matches!(prop("init.svc.adbd").as_deref(), None | Some("stopped")) {
            ensure!(start.elapsed() < timeout, "adbd still runs {} ms after the port went to none", timeout.as_millis());
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}

/// Hand the port to `config` through "none", which stops the programs
/// serving it and takes every function off; init then sets the port up
/// once the programs behind the new configuration are ready. A phone whose
/// init has no rules for `config` is refused before the port is touched, so
/// adb never goes; a switch that fails halfway hands the port back to adb.
pub fn switch(config: &str, timeout: Duration) -> Result<()> {
    // Already there: nothing to take off and on again, and no chance to
    // lose the port on the way.
    if prop("sys.usb.state").as_deref() == Some(config) {
        return Ok(());
    }
    ensure!(
        can_switch(config),
        "init has no rules to hand the port to {config} and back (sys.usb.configfs is {:?}, ffs.mtp is there: {}); adb is left alone",
        prop("sys.usb.configfs"),
        Path::new("/config/usb_gadget/g1/functions/ffs.mtp").exists()
    );
    if let Err(why) = set("none", timeout) {
        let _ = kmsg(&format!("usb: the port did not go through none for {config}: {why:#}"));
        return back_to_adb(why, config, timeout);
    }
    if let Err(why) = set(config, timeout) {
        let _ = kmsg(&format!("usb: the port did not take {config}: {why:#}"));
        return back_to_adb(why, config, timeout);
    }
    Ok(())
}

/// Hand the port back to adb after a failed switch, so a failure never
/// leaves adb gone. Where the port never left adb this is a no-op; where it
/// went through none, a direct hand-over is tried first, and the same two
/// steps a normal switch takes are the fallback.
fn back_to_adb(why: anyhow::Error, config: &str, timeout: Duration) -> Result<()> {
    let restored = if prop("sys.usb.state").as_deref() == Some("adb") {
        Ok(())
    } else {
        match set("adb", timeout) {
            Ok(()) => Ok(()),
            Err(first) => {
                let _ = kmsg(&format!("usb: the port did not take adb straight back: {first:#}"));
                set("none", timeout).and_then(|()| set("adb", timeout))
            }
        }
    };
    match restored {
        Ok(()) => {
            let _ = kmsg(&format!("usb: {config} did not switch; the port is adb"));
            Err(why)
        }
        Err(back) => {
            let _ = kmsg(&format!("usb: {config} did not switch, and the port could not be handed back to adb: {back:#}"));
            Err(anyhow!("{why:#}; and the port could not be handed back to adb: {back:#}"))
        }
    }
}
