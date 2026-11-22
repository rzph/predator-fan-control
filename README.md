# predator-fan-control

A lightweight Linux CLI utility to monitor hardware temperatures, read fan RPMs, and control CPU/GPU fan profiles on the **Acer Predator Helios 300 (PH315-51)** via the Embedded Controller (`ec_sys`).

## Features

- **Live Monitoring**: Displays real-time temperatures from `/sys/class/hwmon` alongside CPU and GPU fan speeds (RPM) read directly from the EC.
- **Fan Profiles**:
  - **Auto** (`-a` / `--auto`): Restores standard BIOS automatic fan control.
  - **Performance** (`-p` / `--performance`): Sets both CPU and GPU fans to maximum speed.
  - **Manual Speed** (`-s` / `--speed <0..100>`): Fixes both fans to a specific duty cycle percentage.
- **Hardware Safety Guard**: Verifies `/sys/class/dmi/id/product_name` matches `PH315-51` before writing to any EC register.

## Prerequisites

Reading and writing to `/sys/kernel/debug/ec/ec0/io` requires root privileges and the `ec_sys` kernel module loaded with write support:

```bash
sudo modprobe ec_sys write_support=1
```

## Installation

```bash
cargo build --release
sudo install -m 755 target/release/predator-fan-control /usr/local/bin/predator-fan-control
```

## Usage

### Monitor temperatures and fan speeds

```bash
sudo predator-fan-control
```

*(Can also be run without `sudo` to monitor `hwmon` temperatures only, if EC debugfs is not accessible).*

### Control fan profiles

```bash
# Restore automatic BIOS fan control
sudo predator-fan-control --auto

# Set fans to maximum speed
sudo predator-fan-control --performance

# Set fans to a fixed manual speed (0 to 100%)
sudo predator-fan-control --speed 70
```

## EC Register Map (PH315-51)

| Function | EC Offset | Value / Format |
| --- | --- | --- |
| CPU Fan Mode | `0x22` | `0x54` (Auto), `0x58` (Max), `0x5C` (Manual) |
| GPU Fan Mode | `0x21` | `0x50` (Auto), `0x60` (Max), `0x70` (Manual) |
| CPU Manual Speed % | `0x37` | `0..=100` (`u8`) |
| GPU Manual Speed % | `0x3A` | `0..=100` (`u8`) |
| CPU Fan RPM | `0x13` | 16-bit little-endian (`u16`) |
| GPU Fan RPM | `0x15` | 16-bit little-endian (`u16`) |

## Credits

- EC mode registers based on [yaroslavyushchenko/acer-predator-ph315-51-fun-control](https://github.com/yaroslavyushchenko/acer-predator-ph315-51-fun-control).
- EC RPM offsets cross-checked with DSDT (`C1FR`/`G1FR`) and [nbfc-linux](https://github.com/nbfc-linux/nbfc-linux).

## License

MIT
