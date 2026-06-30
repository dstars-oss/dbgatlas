# DbgAtlas WiX Installer

This directory contains the first WiX source for a DbgAtlas MSI.

Expected variables:

- `ReleaseDir`: directory containing the complete release payload.
- `ProductVersion`: MSI product version.

The MSI installs payload files into the target user's Local Programs layout:

```text
[LocalAppDataFolder]Programs\dbgatlas\bin
```

This `.wxs` is intentionally scoped as a machine-level service bootstrap because
`dbgatlas service install` registers the `DbgAtlas` Windows service. For managed
or elevated installs, pass `INSTALLROOT` explicitly so repair/uninstall uses the
same user profile path that owns the payload.

Service initialization is delegated to:

```text
dbgatlas.exe service install --payload-mode use-existing --install-root [INSTALLROOT]
```

The Rust service installer owns `etc\runtime.toml`, `etc\token`, `var\log`, and
`bin\rt`. The MSI owns the files it places directly under `bin`.
