#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""Put one wheel of the Python binding together around an extension cargo built.

    wheel.py --crate <binding dir> --python <interpreter> --module <built library>
             --platform <platform tag> --out <dir>

maturin cannot build the binding as it stands, so scripts/package/wheels.sh
builds the extension with cargo, against one interpreter at a time, and this
script writes the wheel around each build. The wheel (the binary
distribution format, PEP 427) holds:

    pulse_wire<EXT_SUFFIX>           the extension, named as the interpreter
                                     imports it (pulse_wire.cpython-312-darwin.so)
    pulse_wire.pyi                   the type stubs, from beside Cargo.toml
    py.typed                         the marker that says they are there (PEP 561)
    pulse_wire-<version>.dist-info/
      METADATA                       Metadata-Version 2.1: Name, Version,
                                     Summary and Requires-Python
      WHEEL                          Wheel-Version 1.0, Generator,
                                     Root-Is-Purelib: false, Tag
      RECORD                         each file's sha256 and size, then RECORD
                                     itself, with neither

Where each value comes from:
  Version          the crate's (Cargo.toml), in its PEP 440 form, converted
                   as scripts/check-versions.py converts it (1.0.0 stays 1.0.0;
                   that check holds pyproject.toml's version to the same form)
  Name, Summary,   pyproject.toml's [project] name, description and
  Requires-Python  requires-python
  Tag              cpXY-cpXY-<platform>, XY the version of the interpreter
                   named, which this script asks, and the ABI tag cpXYt for a
                   free-threaded one; <platform> is the caller's. The
                   interpreter can be another than the one running this
                   script, which needs Python 3.11 or later, for tomllib.

The metadata carries nothing else. The wheel is reproducible: its entries
are in a fixed order, each dated SOURCE_DATE_EPOCH when that is set, else
1980-01-01, the earliest a zip entry can hold. It is written beside its
final name and moved there, so a failed run leaves no partial wheel.
Prints the wheel's path. Standard library only.
"""
import argparse
import base64
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import subprocess
import sys
import time
import zipfile

if sys.version_info < (3, 11):
    sys.exit(f"wheel.py needs Python 3.11 or later (tomllib); this is {sys.version.split()[0]}")
import tomllib

MODULE = "pulse_wire"
GENERATOR = "pulse-mesh scripts/package/wheel.py"

# Asked of the target interpreter: what names and tags its extension.
PROBE = """
import json, sys, sysconfig
print(json.dumps({
    "implementation": sys.implementation.name,
    "version": list(sys.version_info[:2]),
    "ext_suffix": sysconfig.get_config_var("EXT_SUFFIX"),
    "free_threaded": bool(sysconfig.get_config_var("Py_GIL_DISABLED")),
}))
"""


def pep440(version):
    """The crate version's PEP 440 form, by scripts/check-versions.py's own
    conversion, so the wheel's version is the one that check accepts."""
    path = pathlib.Path(__file__).resolve().parents[1] / "check-versions.py"
    spec = importlib.util.spec_from_file_location("check_versions", path)
    check_versions = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(check_versions)
    return check_versions.pep440(version)


def record_hash(data):
    """RECORD's hash form: sha256=, then the urlsafe base64 digest unpadded."""
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=")
    return "sha256=" + digest.decode()


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--crate", required=True, type=pathlib.Path, help="the binding's directory")
    parser.add_argument("--python", required=True, help="the interpreter the extension was built for")
    parser.add_argument("--module", required=True, type=pathlib.Path, help="the library cargo built")
    parser.add_argument("--platform", required=True, help="the platform tag, e.g. manylinux_2_28_x86_64")
    parser.add_argument("--out", required=True, type=pathlib.Path, help="the directory the wheel goes in")
    args = parser.parse_args()

    if not re.fullmatch(r"[a-z0-9_]+", args.platform):
        sys.exit(f"wheel.py: {args.platform!r} is not a platform tag")
    target = json.loads(subprocess.check_output([args.python, "-c", PROBE]))
    if target["implementation"] != "cpython" or not target["ext_suffix"]:
        sys.exit(f"wheel.py: {args.python} is not a CPython with an extension suffix")
    major, minor = target["version"]
    python_tag = f"cp{major}{minor}"
    abi_tag = python_tag + ("t" if target["free_threaded"] else "")
    tag = f"{python_tag}-{abi_tag}-{args.platform}"

    # the repository root: the nearest directory above the crate holding LICENSE
    root = next((d for d in args.crate.resolve().parents if (d / "LICENSE").is_file()), None)
    if root is None:
        sys.exit(f"wheel.py: no LICENSE above {args.crate}")
    crate = tomllib.loads((args.crate / "Cargo.toml").read_text())["package"]
    project = tomllib.loads((args.crate / "pyproject.toml").read_text())["project"]
    version = pep440(crate["version"])
    # The distribution's name as file names spell it (PEP 427, PEP 503).
    dist = re.sub(r"[-_.]+", "_", project["name"]).lower()
    dist_info = f"{dist}-{version}.dist-info"

    metadata = "".join(
        f"{key}: {value}\n"
        for key, value in [
            ("Metadata-Version", "2.1"),
            ("Name", project["name"]),
            ("Version", version),
            ("Summary", project["description"]),
            ("Requires-Python", project["requires-python"]),
            ("License", project.get("license", {}).get("text", "LicenseRef-Proprietary")),
        ]
        + [("Classifier", c) for c in project.get("classifiers", [])]
    )
    wheel = f"Wheel-Version: 1.0\nGenerator: {GENERATOR}\nRoot-Is-Purelib: false\nTag: {tag}\n"

    # (path in the wheel, bytes, mode), in the order the wheel holds them:
    # the package first, the dist-info last, RECORD at its end.
    files = [
        (MODULE + target["ext_suffix"], args.module.read_bytes(), 0o755),
        (MODULE + ".pyi", (args.crate / (MODULE + ".pyi")).read_bytes(), 0o644),
        ("py.typed", b"", 0o644),
        # the repository's license material: the terms, their summary and the third-party notices
        *[(f"{dist_info}/licenses/{name}", (root / name).read_bytes(), 0o644)
          for name in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md") if (root / name).is_file()],
        (f"{dist_info}/METADATA", metadata.encode(), 0o644),
        (f"{dist_info}/WHEEL", wheel.encode(), 0o644),
    ]
    record = "".join(f"{name},{record_hash(data)},{len(data)}\n" for name, data, _ in files)
    record += f"{dist_info}/RECORD,,\n"
    files.append((f"{dist_info}/RECORD", record.encode(), 0o644))

    epoch = max(int(os.environ.get("SOURCE_DATE_EPOCH", 0)), 315532800)  # 1980-01-01 UTC
    date_time = time.gmtime(epoch)[:6]
    args.out.mkdir(parents=True, exist_ok=True)
    path = args.out / f"{dist}-{version}-{tag}.whl"
    partial = path.with_name(path.name + ".partial")
    with zipfile.ZipFile(partial, "w", zipfile.ZIP_DEFLATED) as zf:
        for name, data, mode in files:
            info = zipfile.ZipInfo(name, date_time)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (0o100000 | mode) << 16  # a regular file, with its mode
            zf.writestr(info, data)
    partial.replace(path)
    print(path)


if __name__ == "__main__":
    main()
