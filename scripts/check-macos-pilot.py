"""Check the generated pilot bundle against its native executable requirements."""

import json
import plistlib
import stat
import struct
import sys
from pathlib import Path


def version_tuple(value):
    parts = tuple(int(part) for part in value.split("."))
    return (parts + (0, 0, 0))[:3]


def native_requirements(path):
    with path.open("rb") as binary:
        magic = binary.read(4)
        if magic not in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"):
            raise ValueError(f"Expected a 64-bit Mach-O pilot executable: {path.name}")
        endian = "<" if magic == b"\xcf\xfa\xed\xfe" else ">"
        header = magic + binary.read(28)
        _, cpu, _, _, command_count, command_bytes, _, _ = struct.unpack(endian + "IiiIIIII", header)
        commands = binary.read(command_bytes)
    offset = 0
    minimum_versions = []
    for _ in range(command_count):
        command, size = struct.unpack_from(endian + "II", commands, offset)
        if size < 8 or offset + size > len(commands):
            raise ValueError(f"Invalid Mach-O load command in {path.name}")
        if command == 0x32:  # LC_BUILD_VERSION
            platform, minimum = struct.unpack_from(endian + "II", commands, offset + 8)
            if platform != 1:
                raise ValueError(f"Expected a macOS binary: {path.name}")
            minimum_versions.append(minimum)
        elif command == 0x24:  # LC_VERSION_MIN_MACOSX
            minimum_versions.append(struct.unpack_from(endian + "I", commands, offset + 8)[0])
        offset += size
    if not minimum_versions:
        raise ValueError(f"Missing macOS deployment requirement in {path.name}")
    minimum = max(minimum_versions)
    return {
        "name": path.name,
        "cpuType": hex(cpu),
        "minimumOS": f"{minimum >> 16}.{(minimum >> 8) & 255}.{minimum & 255}",
        "bytes": path.stat().st_size,
    }


def inspect_bundle(bundle):
    contents = bundle / "Contents"
    with (contents / "Info.plist").open("rb") as source:
        info = plistlib.load(source)
    if info.get("CFBundleIdentifier") != "com.meetily.connections-pilot":
        raise ValueError("The pilot must use its isolated application identifier")
    if not info.get("NSMicrophoneUsageDescription"):
        raise ValueError("The pilot is missing its microphone permission description")
    schemes = {
        scheme
        for item in info.get("CFBundleURLTypes", [])
        for scheme in item.get("CFBundleURLSchemes", [])
    }
    if "meetingly" not in schemes:
        raise ValueError("The native OAuth callback scheme is missing")
    minimum_os = info.get("LSMinimumSystemVersion")
    if not minimum_os:
        raise ValueError("The bundle must declare its minimum macOS version")
    binaries = []
    for name in (info["CFBundleExecutable"], "llama-helper", "ffmpeg"):
        path = contents / "MacOS" / name
        if not path.stat().st_mode & stat.S_IXUSR:
            raise ValueError(f"The bundled executable is not executable: {name}")
        requirement = native_requirements(path)
        if version_tuple(requirement["minimumOS"]) > version_tuple(minimum_os):
            raise ValueError(
                f"The bundle advertises macOS {minimum_os}, but {name} requires {requirement['minimumOS']}"
            )
        binaries.append(requirement)
    if len({binary["cpuType"] for binary in binaries}) != 1:
        raise ValueError("The pilot and its sidecars have different architectures")
    return {
        "identifier": info["CFBundleIdentifier"],
        "minimumOS": minimum_os,
        "callbackScheme": "meetingly",
        "microphonePermission": True,
        "binaries": binaries,
    }


if __name__ == "__main__":
    print(json.dumps(inspect_bundle(Path(sys.argv[1])), indent=2))
