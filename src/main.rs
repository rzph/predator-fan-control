use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const EC_IO_PATH: &str = "/sys/kernel/debug/ec/ec0/io";
const PRODUCT_NAME_PATH: &str = "/sys/class/dmi/id/product_name";
const HWMON_PATH: &str = "/sys/class/hwmon";
const SUPPORTED_MODEL: &str = "PH315-51";
const MAX_PLAUSIBLE_RPM: u16 = 10_000;

/* EC mode registers and values reverse-engineered for the Acer Predator PH315-51.
 * Mode offsets: https://github.com/yaroslavyushchenko/acer-predator-ph315-51-fun-control
 * RPM offsets (DSDT C1FR/G1FR): https://github.com/nbfc-linux/nbfc-linux/blob/main/share/nbfc/configs/Acer%20Predator%20PH315-53.json
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FanProfile {
    Auto,
    Performance,
    Manual(u8),
}

impl FanProfile {
    fn writes(self) -> &'static [(u64, u8)] {
        match self {
            Self::Auto => &[(0x22, 0x54), (0x21, 0x50)],
            Self::Performance => &[(0x22, 0x58), (0x21, 0x60)],
            Self::Manual(_) => &[(0x22, 0x5C), (0x21, 0x70)],
        }
    }

    fn apply(self) -> io::Result<()> {
        let mut ec = OpenOptions::new().read(true).write(true).open(EC_IO_PATH)?;
        self.apply_to(&mut ec)
    }

    fn apply_to<W: Write + Seek>(self, ec: &mut W) -> io::Result<()> {
        for &(addr, val) in self.writes() {
            ec.seek(SeekFrom::Start(addr))?;
            ec.write_all(&[val])?;
        }
        if let Self::Manual(percent) = self {
            for addr in [0x37, 0x3A] {
                ec.seek(SeekFrom::Start(addr))?;
                ec.write_all(&[percent])?;
            }
        }
        Ok(())
    }

    fn summary(self) -> String {
        match self {
            Self::Auto => "Fan profile applied: Auto".into(),
            Self::Performance => "Fan profile applied: Performance (max)".into(),
            Self::Manual(pct) => format!(
                "Fan speed set to {pct}% (fixed manual mode; use -a to return to automatic control)."
            ),
        }
    }
}

struct Hwmon {
    path: PathBuf,
    name: String,
}

impl Hwmon {
    fn discover() -> Vec<Self> {
        Self::discover_in(Path::new(HWMON_PATH))
    }

    fn discover_in(dir: &Path) -> Vec<Self> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut hwmons: Vec<_> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let name = fs::read_to_string(path.join("name")).ok()?.trim().to_string();
                Some(Self { path, name })
            })
            .collect();
        hwmons.sort_by(|a, b| a.name.cmp(&b.name));
        hwmons
    }

    fn temperatures(&self) -> Vec<(String, f32)> {
        (1..=10)
            .filter_map(|i| {
                let raw = fs::read_to_string(self.path.join(format!("temp{i}_input"))).ok()?;
                let celsius = raw.trim().parse::<f32>().ok()? / 1000.0;
                let label = fs::read_to_string(self.path.join(format!("temp{i}_label")))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("temp{i}"));
                Some((label, celsius))
            })
            .collect()
    }
}

fn read_ec_rpms() -> Option<(Option<u16>, Option<u16>)> {
    let mut ec = File::open(EC_IO_PATH).ok()?;
    Some(read_ec_rpms_from(&mut ec))
}

fn read_ec_rpms_from<R: Read + Seek>(ec: &mut R) -> (Option<u16>, Option<u16>) {
    let mut read_word = |addr: u64| -> Option<u16> {
        ec.seek(SeekFrom::Start(addr)).ok()?;
        let mut buf = [0u8; 2];
        ec.read_exact(&mut buf).ok()?;
        let rpm = u16::from_le_bytes(buf);
        (rpm <= MAX_PLAUSIBLE_RPM).then_some(rpm)
    };
    (read_word(0x13), read_word(0x15))
}

/// Turns a failed EC access into an actionable message for the user.
fn explain_ec_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => format!(
            "{EC_IO_PATH} not found: load the EC module with 'sudo modprobe ec_sys write_support=1'"
        ),
        io::ErrorKind::PermissionDenied => {
            format!("permission denied on {EC_IO_PATH}: run with sudo (and check Secure Boot lockdown)")
        }
        _ => format!("EC access failed: {error}"),
    }
}

fn is_supported_model() -> bool {
    is_supported_model_at(Path::new(PRODUCT_NAME_PATH))
}

fn is_supported_model_at(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|m| m.to_ascii_uppercase().contains(SUPPORTED_MODEL))
}

fn render_dashboard(hwmons: &[Hwmon]) {
    print!("\x1B[2J\x1B[1;1H");
    let _ = io::stdout().flush();

    println!("========================================");
    println!("          PREDATOR FAN CONTROL          ");
    println!("========================================\n");

    for hwmon in hwmons {
        let temps = hwmon.temperatures();
        if temps.is_empty() {
            continue;
        }
        println!("-- {} --", hwmon.name.to_uppercase());
        for (label, celsius) in temps {
            println!("   {label:<15} {celsius:>6.1} °C");
        }
        println!();
    }

    println!("-- FANS --");
    match read_ec_rpms() {
        Some((cpu, gpu)) => {
            for (name, rpm) in [("CPU Fan", cpu), ("GPU Fan", gpu)] {
                match rpm {
                    Some(v) => println!("   {name:<15} {v:>6} RPM"),
                    None => println!("   {name:<15}    N/A"),
                }
            }
        }
        None => println!("   RPM unavailable (run with sudo and ec_sys loaded)"),
    }
    println!("\n========================================");
}

fn parse_profile(args: &[String]) -> Result<Option<FanProfile>, &'static str> {
    match args {
        [] => Ok(None),
        [f] if f == "-a" || f == "--auto" => Ok(Some(FanProfile::Auto)),
        [f] if f == "-p" || f == "--performance" => Ok(Some(FanProfile::Performance)),
        [f, val] if f == "-s" || f == "--speed" => val
            .parse::<u8>()
            .ok()
            .filter(|&v| v <= 100)
            .map(|v| Some(FanProfile::Manual(v)))
            .ok_or("Speed must be an integer between 0 and 100."),
        _ => Err("Unrecognized arguments."),
    }
}

fn print_usage() {
    eprintln!(
        "Usage: sudo predator-fan-control [-a | --auto | -p | --performance | -s <0..100> | --speed <0..100>]"
    );
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return;
    }

    if !is_supported_model() {
        eprintln!("Error: only the Acer Predator {SUPPORTED_MODEL} is supported.");
        std::process::exit(2);
    }

    match parse_profile(&args) {
        Ok(Some(profile)) => match profile.apply() {
            Ok(()) => println!("{}", profile.summary()),
            Err(e) => {
                eprintln!("Failed to apply fan profile {profile:?}: {}", explain_ec_error(&e));
                std::process::exit(1);
            }
        },
        Ok(None) => {
            let hwmons = Hwmon::discover();
            if hwmons.is_empty() {
                eprintln!("No hwmon component found.");
            }
            loop {
                render_dashboard(&hwmons);
                thread::sleep(Duration::from_secs(1));
            }
        }
        Err(msg) => {
            eprintln!("Error: {msg}");
            print_usage();
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_str(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parses_valid_profiles() {
        assert_eq!(parse_profile(&vec_str(&[])), Ok(None));
        assert_eq!(parse_profile(&vec_str(&["-a"])), Ok(Some(FanProfile::Auto)));
        assert_eq!(
            parse_profile(&vec_str(&["--performance"])),
            Ok(Some(FanProfile::Performance))
        );
        assert_eq!(
            parse_profile(&vec_str(&["--speed", "75"])),
            Ok(Some(FanProfile::Manual(75)))
        );
    }

    #[test]
    fn rejects_invalid_profiles() {
        assert!(parse_profile(&vec_str(&["--speed", "101"])).is_err());
        assert!(parse_profile(&vec_str(&["--speed"])).is_err());
        assert!(parse_profile(&vec_str(&["-a", "extra"])).is_err());
        assert!(parse_profile(&vec_str(&["--unknown"])).is_err());
    }

    #[test]
    fn explains_ec_write_errors() {
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert!(explain_ec_error(&missing).contains("modprobe ec_sys write_support=1"));

        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(explain_ec_error(&denied).contains("sudo"));
    }

    #[test]
    fn applies_profiles_to_ec_buffer() {
        let mut ec = io::Cursor::new(vec![0u8; 256]);

        FanProfile::Auto.apply_to(&mut ec).unwrap();
        assert_eq!(ec.get_ref()[0x22], 0x54);
        assert_eq!(ec.get_ref()[0x21], 0x50);

        FanProfile::Performance.apply_to(&mut ec).unwrap();
        assert_eq!(ec.get_ref()[0x22], 0x58);
        assert_eq!(ec.get_ref()[0x21], 0x60);

        FanProfile::Manual(65).apply_to(&mut ec).unwrap();
        assert_eq!(ec.get_ref()[0x22], 0x5C);
        assert_eq!(ec.get_ref()[0x21], 0x70);
        assert_eq!(ec.get_ref()[0x37], 65);
        assert_eq!(ec.get_ref()[0x3A], 65);
    }

    #[test]
    fn reads_ec_rpms_and_rejects_implausible_values() {
        let mut buf = vec![0u8; 256];
        // CPU @ 0x13 = 2400 RPM (0x0960 -> [0x60, 0x09])
        buf[0x13..=0x14].copy_from_slice(&[0x60, 0x09]);
        // GPU @ 0x15 = 5100 RPM (0x13EC -> [0xEC, 0x13])
        buf[0x15..=0x16].copy_from_slice(&[0xEC, 0x13]);

        let mut ec = io::Cursor::new(buf);
        assert_eq!(read_ec_rpms_from(&mut ec), (Some(2400), Some(5100)));

        // Corrupt GPU RPM to 0xFFFF (65535 > 10_000)
        ec.get_mut()[0x15..=0x16].copy_from_slice(&[0xFF, 0xFF]);
        assert_eq!(read_ec_rpms_from(&mut ec), (Some(2400), None));
    }

    #[test]
    fn discovers_hwmon_temperatures_and_checks_model() {
        let root = env::temp_dir().join(format!("predator_tdd_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let hwmon0 = root.join("hwmon0");
        let hwmon1 = root.join("hwmon1");
        fs::create_dir_all(&hwmon0).unwrap();
        fs::create_dir_all(&hwmon1).unwrap();

        fs::write(hwmon0.join("name"), "coretemp\n").unwrap();
        fs::write(hwmon0.join("temp1_input"), "54500\n").unwrap();
        fs::write(hwmon0.join("temp1_label"), "Package id 0\n").unwrap();
        fs::write(hwmon0.join("temp2_input"), "52000\n").unwrap();

        fs::write(hwmon1.join("name"), "acpitz\n").unwrap();
        fs::write(hwmon1.join("temp1_input"), "48000\n").unwrap();

        let hwmons = Hwmon::discover_in(&root);
        assert_eq!(hwmons.len(), 2);
        assert_eq!(hwmons[0].name, "acpitz");
        assert_eq!(hwmons[1].name, "coretemp");
        assert_eq!(
            hwmons[1].temperatures(),
            vec![
                ("Package id 0".to_string(), 54.5),
                ("temp2".to_string(), 52.0),
            ]
        );

        let model_file = root.join("product_name");
        fs::write(&model_file, "Predator PH315-51\n").unwrap();
        assert!(is_supported_model_at(&model_file));

        fs::write(&model_file, "ThinkPad T480\n").unwrap();
        assert!(!is_supported_model_at(&model_file));

        let _ = fs::remove_dir_all(&root);
    }
}
