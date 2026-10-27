<img src="docs/icon.png" alt="usbip icon" width="128" height="128">

# usbip-macos

A [USB/IP](https://usbip.sourceforge.net/) client for macOS.

## Permissions

Because this tool uses `IOUSBHostControllerInterface` it either needs to be signed with the
`com.apple.developer.usb.host-controller-interface` entitlement (which I don't have yet) or run as
root with SIP disabled and the `amfi_get_out_of_my_way=1` boot-arg set.

## Usage

### Mac app

Opening `usbip.app` puts an icon in the menu bar. Use **Set Server…** to pick the server,
then click an exported device to attach it and click it again to detach it. Quitting detaches
everything.

The app runs as your user, so it only works in a build signed with the entitlement. Without one,
use the CLI as root.

### CLI

```sh
sudo usbip-macos -r <server> list
sudo usbip-macos -r <server> attach --busid 1-1.4
```

The device stays attached until you stop the client.

## Limitations

- Isochronous devices (most webcams, microphones and speakers) don't work. This is a macOS
  limitation: `IOUSBHostControllerInterface` doesn't properly support isochronous transfers.
- UAS disks work, but only as plain mass storage. This is also a macOS limitation:
  `IOUSBHostControllerInterface` doesn't support the bulk streams UAS needs.
