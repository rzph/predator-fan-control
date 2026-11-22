use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

#[derive(Debug)]
struct Hwmon {
    path: PathBuf,
    name: String,
}

#[derive(Debug)]
struct Sensor {
    value: f32,
    name: String,
}

/* EC mode registers and values reverse-engineered for the Acer Predator
PH315-51. Source: https://github.com/yaroslavyushchenko/acer-predator-ph315-51-fun-control */

const CPU_MODE_ADDR: u64 = 0x22;
const GPU_MODE_ADDR: u64 = 0x21;
const CPU_AUTO_MODE: u8 = 0x54;
const GPU_AUTO_MODE: u8 = 0x50;
const CPU_MAX_FAN_MODE: u8 = 0x58;
const GPU_MAX_FAN_MODE: u8 = 0x60;
const CPU_MANUAL_MODE: u8 = 0x5C;
const GPU_MANUAL_MODE: u8 = 0x70;
const CPU_FAN_SPEED_ADDR: u64 = 0x37;
const GPU_FAN_SPEED_ADDR: u64 = 0x3A;

/*The PH315-51 DSDT labels 16-bit fields C1FR/G1FR at offsets 0x13/0x15
in its ECMM SystemMemory region. NBFC reads the same offsets as
little-endian EC words on related G3-572/PH315-53 models:
https://github.com/nbfc-linux/nbfc-linux/blob/main/share/nbfc/configs/Acer%20Predator%20PH315-53.json
This is a strong cross-check, but not a live probe of this laptop's BIOS revision.*/
const CPU_FAN_RPM_ADDR: u64 = 0x13;
const GPU_FAN_RPM_ADDR: u64 = 0x15;

#[derive(Debug, Clone, Copy)]
enum FanProfile {
    Auto,
    Performance,
    Manual(u8),
}

impl FanProfile {
    fn apply(&self) -> io::Result<()> {
        match self {
            FanProfile::Auto => {
                write_ec_register(CPU_MODE_ADDR, CPU_AUTO_MODE)?;
                write_ec_register(GPU_MODE_ADDR, GPU_AUTO_MODE)?;
            }
            FanProfile::Performance => {
                write_ec_register(CPU_MODE_ADDR, CPU_MAX_FAN_MODE)?;
                write_ec_register(GPU_MODE_ADDR, GPU_MAX_FAN_MODE)?;
            }
            FanProfile::Manual(percent) => {
                write_ec_register(CPU_MODE_ADDR, CPU_MANUAL_MODE)?;
                write_ec_register(GPU_MODE_ADDR, GPU_MANUAL_MODE)?;
                write_ec_register(CPU_FAN_SPEED_ADDR, *percent)?;
                write_ec_register(GPU_FAN_SPEED_ADDR, *percent)?;
            }
        }
        Ok(())
    }
}

// Hardware write function (Embedded Controller)
fn write_ec_register(address: u64, value: u8) -> std::io::Result<()> {
    let mut ec_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/sys/kernel/debug/ec/ec0/io")?;
    ec_file.seek(SeekFrom::Start(address))?;
    ec_file.write_all(&[value])?;
    Ok(())
}

fn read_ec_rpm(address: u64) -> Option<u16> {
    let mut file = File::open("/sys/kernel/debug/ec/ec0/io").ok()?;
    file.seek(SeekFrom::Start(address)).ok()?;

    let mut bytes = [0u8; 2];
    file.read_exact(&mut bytes).ok()?;
    let rpm = u16::from_le_bytes(bytes);

    // Reject obvious non-RPM values if the EC map differs on this BIOS.
    (rpm <= 10_000).then_some(rpm)
}

fn read_file(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn is_supported_model() -> bool {
    read_file(Path::new("/sys/class/dmi/id/product_name"))
        .map(|model| model.to_ascii_uppercase().contains("PH315-51"))
        .unwrap_or(false)
}

fn find_hwmons(dir_path: &Path) -> Vec<Hwmon> {
    let mut hwmons = Vec::new();
    let Ok(entries) = fs::read_dir(dir_path) else {
        return hwmons;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = read_file(&path.join("name")) else {
            continue;
        };
        hwmons.push(Hwmon {
            path,
            name: name.trim().to_string(),
        });
    }
    hwmons
}

fn read_temperature(path: &Path) -> Option<f32> {
    let value = read_file(path)?;
    let millidegrees: f32 = value.trim().parse().ok()?;
    Some(millidegrees / 1000.0)
}

fn read_sensors(hwmon: &Hwmon) -> Vec<Sensor> {
    let mut sensors = Vec::new();
    for i in 1..=10 {
        let input = hwmon.path.join(format!("temp{}_input", i));
        if let Some(value) = read_temperature(&input) {
            let label_path = hwmon.path.join(format!("temp{}_label", i));
            let name = read_file(&label_path)
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| format!("temp{}", i));
            sensors.push(Sensor { name, value });
        }
    }
    sensors
}

fn read_fan_speeds(hwmon: &Hwmon) -> Vec<(String, u32)> {
    let mut fans = Vec::new();
    for i in 1..=8 {
        let input = hwmon.path.join(format!("fan{}_input", i));
        let Some(rpm) = read_file(&input).and_then(|value| value.trim().parse().ok()) else {
            continue;
        };

        let label_path = hwmon.path.join(format!("fan{}_label", i));
        let name = read_file(&label_path)
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| format!("{} fan{}", hwmon.name, i));
        fans.push((name, rpm));
    }
    fans
}

fn display(hwmons: &[Hwmon]) {
    print!("\x1B[2J\x1B[1;1H");
    io::stdout().flush().unwrap();

    println!("========================================");
    println!("           PREDATOR CONTROL             ");
    println!("========================================");
    println!();

    // 1. Display temperatures via hwmon (standard ACPI)
    for hwmon in hwmons {
        let sensors = read_sensors(hwmon);
        if sensors.is_empty() {
            continue;
        }

        println!("-- {} --", hwmon.name.to_uppercase());
        for sensor in sensors {
            println!("   {:<15} {:>6.1} °C", sensor.name, sensor.value);
        }
        println!();
    }

    println!("-- VENTILATEURS --");
    let (cpu_rpm, gpu_rpm) = if is_supported_model() {
        (read_ec_rpm(CPU_FAN_RPM_ADDR), read_ec_rpm(GPU_FAN_RPM_ADDR))
    } else {
        (None, None)
    };

    if cpu_rpm.is_some() || gpu_rpm.is_some() {
        match cpu_rpm {
            Some(rpm) => println!("   CPU Fan         {:>6} RPM", rpm),
            None => println!("   CPU Fan              N/D"),
        }
        match gpu_rpm {
            Some(rpm) => println!("   GPU Fan         {:>6} RPM", rpm),
            None => println!("   GPU Fan              N/D"),
        }
    } else {
        let fans: Vec<_> = hwmons.iter().flat_map(read_fan_speeds).collect();
        if fans.is_empty() {
            println!("   RPM indisponibles (EC inaccessible, aucun fan*_input hwmon)");
        } else {
            for (name, rpm) in fans {
                println!("   {:<15} {:>6} RPM", name, rpm);
            }
        }
    }
    println!();
    println!("========================================");
}
{"*******", "*d----*", "**---a*", "*******"};
fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        let profile = match args[1].as_str() {
            "-p" | "--performance" => Some(FanProfile::Performance),
            "-a" | "--auto" => Some(FanProfile::Auto),
            "--speed" | "-s" if args.len() > 2 => args[2]
                .parse::<u8>()
                .ok()
                .filter(|speed| *speed <= 100)
                .map(FanProfile::Manual),
            "-set" if args.len() > 2 => match args[2].as_str() {
                "performance" => Some(FanProfile::Performance),
                "auto" => Some(FanProfile::Auto),
                _ => None,
            },
            _ => None,
        };

        match profile {
            Some(p) => {
                if !is_supported_model() {
                    eprintln!(
                        "Refus d'écrire dans l'EC : seul l'Acer Predator PH315-51 est pris en charge."
                    );
                    std::process::exit(2);
                }
                match p.apply() {
                    Ok(()) => match p {
                        FanProfile::Manual(percent) => println!(
                            "Ventilateurs réglés à {} % (mode manuel fixe ; utilisez -a pour revenir à la régulation automatique).",
                            percent
                        ),
                        _ => println!("Fan profile applied: {:?}", p),
                    },
                    Err(error) => {
                        eprintln!("Impossible d'appliquer le profil {:?} : {}", p, error);
                        std::process::exit(1);
                    }
                }
            }
            None => {
                eprintln!("Error: unrecognized hardware argument.");
                eprintln!(
                    "Usage: sudo predatorfc [-a | -p | --speed 0..100 | -set auto | -set performance]"
                );
            }
        }
        return;
    }

    let target_path = Path::new("/sys/class/hwmon");
    let hwmons = find_hwmons(target_path);

    if hwmons.is_empty() {
        eprintln!("No hwmon component found.");
        // Keep the UI running so it can report that sensor data is unavailable.
    }

    loop {
        display(&hwmons);
        thread::sleep(Duration::from_secs(1));
    }
}
