# DbgAtlas WiX Installer

This directory contains the first WiX source for a DbgAtlas MSI.

Expected variables:

- `ReleaseDir`: directory containing the complete release payload.
- `ProductVersion`: MSI product version.

Build this package as x64 so the Program Files directory and payload component
bitness are consistent:

```powershell
wix build -arch x64 DbgAtlas.wxs -d ReleaseDir=<release-payload> -d ProductVersion=<version> -o DbgAtlas-<version>-x64.msi
```

The MSI installs payload files into a machine-wide Program Files layout:

```text
[ProgramFiles64Folder]DbgAtlas\bin
```

This `.wxs` is intentionally scoped as a machine-level service bootstrap because
`dbgatlas service install` registers the `DbgAtlas` Windows service. Managed
installs may pass `INSTALLROOT` explicitly to use another machine-wide install
root.

Service initialization is delegated to:

```text
dbgatlas.exe service install --payload-mode use-existing --install-root [INSTALLROOT]
```

The Rust service installer owns `etc\runtime.toml`, `etc\token`, `var\log`, and
`bin\rt`. The MSI owns the files it places directly under `bin`.
