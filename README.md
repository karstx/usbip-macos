<img src="docs/icon.png" alt="usbip icon" width="128" height="128">

# usbip-macos

A [USB/IP](https://usbip.sourceforge.net/) client for macOS.

## Permissions

Attaching a device uses `IOUSBHostControllerInterface`, which needs one of:

- an app bundle signed with the `com.apple.developer.usb.host-controller-interface` entitlement
  and a provisioning profile that grants it (which I don't have yet)
- SIP disabled, running as root
- SIP disabled, the `amfi_get_out_of_my_way=1` boot-arg, and an ad-hoc signature claiming the
  entitlement: `codesign --force --sign - --entitlements entitlements.plist usbip.app`

Listing a server's devices needs none of this.

## Usage

### Mac app

Opening `usbip.app` puts an icon in the menu bar. Add a server, then click an exported device to
attach it and click it again to detach it.

- **Settings…** (⌘,) holds the server list (add, rename by double-clicking, remove), **Open at
  Login**, and a log of what the app has been doing.
- Attached devices stay attached: if the connection drops, the app reconnects on its own, and
  it attaches them again after sleep and on the next launch. Detaching a device by clicking it
  is what makes the app forget it. **Try Again** in the menu skips the wait before the next
  attempt.
- Detaching a disk ejects its volumes first. If a volume is in use the device stays attached
  and the app says why.
- The icon is dimmed while nothing is attached and crossed out while a device is reconnecting.
- Notifications tell you when a device disconnects, reconnects or fails to attach.
- The menu shows when a newer release is out on GitHub.
- Logs also go to the unified log, under the subsystem `net.karstx.usbip`:
  `log stream --predicate 'subsystem == "net.karstx.usbip"'`.

The app runs as your user, so it only works in a build signed with the entitlement. Without one,
it says so at the top of the menu; use the CLI as root instead.

### CLI

```sh
usbip-macos -r <server> list
sudo usbip-macos -r <server> attach --busid 1-1.4
```

The device stays attached until you stop the client.

## Limitations

- Isochronous devices (most webcams, microphones and speakers) don't work. This is a macOS
  limitation: `IOUSBHostControllerInterface` doesn't properly support isochronous transfers.
- UAS disks work, but only as plain mass storage. This is also a macOS limitation:
  `IOUSBHostControllerInterface` doesn't support the bulk streams UAS needs.

## Releases

Pushing a `v*` tag builds a draft GitHub release with the dmg and a tarball of the CLI. The tag
must match the version in `Cargo.toml`, since the app compares the two to offer updates.

CI signs the app when these repository secrets are set, and signs it ad-hoc otherwise:

| Secret | Contents |
| --- | --- |
| `DEVELOPER_ID_P12`, `DEVELOPER_ID_PASSWORD` | Base64 of a Developer ID Application identity exported as .p12, and its password |
| `PROVISIONING_PROFILE` | Base64 of the provisioning profile that grants the host controller entitlement |
| `NOTARY_KEY`, `NOTARY_KEY_ID`, `NOTARY_ISSUER` | Base64 of an App Store Connect API key (.p8), its key id and issuer id; tags are notarized with it |
