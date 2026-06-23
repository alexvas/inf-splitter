# Delta: Deployment & Packaging

**Change ID:** `fix-interactions-diagnostics-gaps`
**Affects:** `debian/postinst`, `debian/inf-splitter.service`

---

## MODIFIED

### Requirement: Linux Package (.deb)

**Updated:** The `.deb` postinst now also creates the session state directory.

The `.deb` package installs:
- Binary: `/usr/bin/inf-splitter`
- Config: `/etc/inf-splitter/inf-splitter.toml`
- Env template: `/etc/inf-splitter/environment`
- Log directory: `/var/log/inf-splitter` (owned by `inf-splitter:inf-splitter`)
- **Session directory: `/var/lib/inf-splitter` (owned by `inf-splitter:inf-splitter`)**
- systemd unit: enabled and started on install

The systemd unit allows writes to `/var/lib/inf-splitter` via `ReadWritePaths` so the service can persist session state at runtime under `ProtectSystem=strict`.

#### Scenario: Fresh install creates session directory
- GIVEN `dpkg -i inf-splitter_*.deb` is run on a system without `/var/lib/inf-splitter/`
- WHEN postinst executes
- THEN `/var/lib/inf-splitter/` is created with owner `inf-splitter:inf-splitter`
- AND the service can write `interactions-sessions.toml` at runtime

#### Scenario: Session directory survives upgrade
- GIVEN `dpkg -i inf-splitter_*.deb` is run on a system where `/var/lib/inf-splitter/` already exists
- WHEN postinst executes
- THEN `mkdir -p` is a no-op
- AND existing session data is preserved
