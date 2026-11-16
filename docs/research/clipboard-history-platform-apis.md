# Clipboard history app: platform APIs, Tauri 2 fit, and distribution

Researched 2026-11-12. I used primary sources only: protocol XML, compositor and app source code, Apple, Microsoft, freedesktop, GNOME, Tauri and crate docs. Where a GitHub repository is cited, I read the default branch as it stood on 2026-11-12. GNOME's GitLab, freedesktop's GitLab and the freedesktop wiki sit behind bot walls (Anubis or go-away), so for GNOME code I read the official GitHub mirrors (`github.com/GNOME/*`). For protocol text I read the copies that `wayland-protocols 1.45` (Ubuntu package `1.45-1~ubuntu0.24.04.2`) and `libwayland` install under `/usr/share`. Some facts come from the machine this was written on (Ubuntu 24.04.5, GNOME Shell 46.0, mutter 46.2, currently in an **X11** session); those are marked **(local check)**. My own conclusions are marked **(inference)**, and anything I couldn't confirm in a primary source is marked **(unverified)**.

---

## TL;DR

**Stack: Tauri 2 is a fine default. Keep it.** None of the hard problems below are about the UI toolkit. They are platform glue you write in Rust whatever shell you pick. Electron wouldn't help: its `clipboard` module has no change event at all, only `readText`/`writeText`/`read`/`write`/`has`/`clear` ([Electron clipboard](https://www.electronjs.org/docs/latest/api/clipboard)). But Tauri's official plugins give you very little of what a clipboard manager needs:

- `tauri-plugin-clipboard-manager` 2.5.0 only reads and writes. It has `write_text`, `write_image`, `read_text`, `write_html`, `clear`, `read_image` and `write_secret`, and **no change monitoring** ([desktop.rs](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/clipboard-manager/src/desktop.rs)).
- `tauri-plugin-global-shortcut` 2.4.1 wraps `global-hotkey` 0.8. That crate (now at 0.10.0) supports "Windows, macOS, Linux (X11 Only)" ([global-hotkey README](https://github.com/tauri-apps/global-hotkey); [plugin Cargo.toml](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/global-shortcut/Cargo.toml)).

**Recommended architecture**

1. **Tauri 2 shell.** It handles the window/popup UI, tray, autostart, single-instance, updater and SQLite history, and you already ship this setup in prayer-times.
2. **A Rust `ClipboardBackend` trait** with one implementation per platform/session. Each one emits "selection changed" and offers `read(formats)` and `write(item)`:
   - **Windows:** a hidden window registered with `AddClipboardFormatListener`, reacting to `WM_CLIPBOARDUPDATE` ([Using the Clipboard](https://learn.microsoft.com/en-us/windows/win32/dataxchg/using-the-clipboard)). Honor `ExcludeClipboardContentFromMonitorProcessing` and `CanIncludeInClipboardHistory` ([Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats)). `clipboard-master` already provides the listener.
   - **macOS:** poll `NSPasteboard.general.changeCount` every 250–500 ms. Maccy and Clipy both do exactly this. Skip `org.nspasteboard.*` marker types, and handle the macOS 15.4+ pasteboard-access alert (see below).
   - **Linux: three backends, chosen at runtime.**
     - (a) `ext-data-control-v1` / `wlr-data-control` through `wl-clipboard-rs`'s `watch` module, for KDE, Sway, Hyprland, COSMIC, niri and similar.
     - (b) On **GNOME Wayland**, a small companion **GNOME Shell extension** that listens to Mutter's `MetaSelection` `owner-changed` signal and exposes the clipboard over session D-Bus. This is exactly what CopyQ ships. Your app talks to it with `zbus`.
     - (c) An **X11/XWayland XFixes** backend (`x11rb`) for X11 sessions. It is also a no-extension fallback on GNOME Wayland, because Mutter mirrors every Wayland selection into Xwayland.
3. **Hotkey layer.**
   - Windows/macOS/X11: the Tauri global-shortcut plugin.
   - Wayland: the **GlobalShortcuts portal** via `ashpd`. It is available on GNOME 48+ and Ubuntu 25.04+.
   - Ubuntu 24.04 Wayland has no GlobalShortcuts backend. There, fall back to a GNOME custom shortcut that runs `yourapp --toggle`, and let the single-instance plugin forward it to the running app.
4. **Paste-back layer.** Write the chosen item to the clipboard, then synthesize Ctrl/Cmd+V where the platform allows it:
   - Windows: `SendInput`.
   - macOS: `CGEvent` post, which needs Accessibility permission.
   - X11: XTest.
   - Wayland: there's no unprivileged way. "Copy and let the user press Ctrl+V" is the honest default. Optional extras are a RemoteDesktop-portal session (it shows a consent dialog) or injection done by the GNOME extension **(unverified)**.

**Biggest risks, in order**

1. **GNOME Wayland (Ubuntu's default) has no clipboard-manager protocol.** Mutter implements neither `ext-data-control-v1` nor `wlr-data-control`.
   - wayland.app lists Mutter 51 as unsupported for both ([ext](https://wayland.app/protocols/ext-data-control-v1), [wlr](https://wayland.app/protocols/wlr-data-control-unstable-v1)).
   - The `src/wayland/` tree of mutter `main` (version 51.1) has no data-control source file ([mutter tree](https://github.com/GNOME/mutter/tree/main/src/wayland), [meson.build](https://github.com/GNOME/mutter/blob/main/meson.build)).
   - The **(local check)** mutter 46 library on Ubuntu 24.04 exports no `data_control` symbols.
   - A plain Wayland client only gets `wl_data_device.selection` events while it has keyboard focus ([wayland.xml](https://gitlab.freedesktop.org/wayland/wayland/-/blob/main/protocol/wayland.xml)).
   - Your workable options are therefore a Shell extension (most reliable, but it is a second artifact you ship and keep compatible with each GNOME release) or forcing your monitor onto XWayland (no extension needed, but it depends on Xwayland and on Mutter's bridge).
   - Ubuntu 25.10 and 26.04 dropped the GNOME X.org session ([25.10 notes](https://documentation.ubuntu.com/release-notes/25.10/), [26.04 notes](https://documentation.ubuntu.com/release-notes/26.04/summary-for-lts-users/)), so "tell users to log into X11" is no longer an answer.
2. **Super+V is already taken on GNOME.** `toggle-message-tray` defaults to `['<Super>v','<Super>m']` ([gnome-shell gschema](https://github.com/GNOME/gnome-shell/blob/main/data/org.gnome.shell.gschema.xml.in), and **(local check)**). On Windows, Win-key shortcuts "are reserved for use by the operating system" ([RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)). Plan a different default, such as Ctrl+Shift+V or Super+Shift+V, and let users rebind it.
3. **Auto-paste on Wayland.** Synthesizing keystrokes needs the RemoteDesktop portal, which "presents a dialog to the user" on `Start()` ([RemoteDesktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)), or a compositor-side helper.
4. **macOS pasteboard privacy.**
   - Since macOS 15.4 the general pasteboard's default behavior is "to ask upon programmatic access", and users can set an app to Always Allow, Ask or Always Deny ([AccessBehavior.default](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum/default)).
   - The developer-preview switch `EnablePasteboardPrivacyDeveloperPreview` "has been removed" in macOS 27.2 ([macOS 27.2 release notes](https://developer.apple.com/documentation/macos-release-notes/macos-27_2-release-notes)).
   - Whether the alert now fires for every app by default is **(unverified)**. Design as if it does: your app will trigger it on its first read, and the user must pick "Always Allow".
5. **GNOME extension review rules.** Extensions that touch the clipboard "MUST declare it in the description", "MUST NOT share clipboard data with a third-party without explicit user interaction", and "MUST NOT ship with default keyboard shortcuts for interacting with clipboard data" ([EGO review guidelines](https://gjs.guide/extensions/review-guidelines/review-guidelines.html)). A bridge extension that feeds your own app should comply, but get it reviewed early.

---

## 1. Watching the clipboard

### 1.1 Linux X11

- **CLIPBOARD vs PRIMARY.** ICCCM: "The selection named by the atom CLIPBOARD is used to hold data that is being transferred between clients, that is, data that usually is being cut and then pasted or copied and then pasted." PRIMARY "is the principal means of communication between clients that use the selection mechanism", which in practice is select-to-copy and middle-click paste ([ICCCM §2.6.1](https://www.x.org/releases/X11R7.6/doc/xorg-docs/specs/ICCCM/icccm.html)). A history app should record CLIPBOARD by default and make PRIMARY opt-in, since PRIMARY changes on every text selection **(inference)**.
- **Change notification: XFixes.**
  - XFixes exists because "Applications wishing to monitor the contents of current selections must poll for selection changes. XFIXES improves this by providing an event delivered whenever the selection ownership changes."
  - `SelectSelectionInput(window, selection, event-mask)` takes a mask from `{ SetSelectionOwner, SelectionWindowDestroy, SelectionClientClose }` and delivers `SelectionNotify` events carrying the subtype, owner and timestamps ([fixesproto](https://www.x.org/archive/current/doc/fixesproto/fixesproto.txt)).
  - `clipboard-master`'s X11 backend requests all three masks via `x11rb` ([x11.rs](https://github.com/DoumanAsh/clipboard-master/blob/master/src/master/linux/x11.rs)).
- **Ownership disappears when the source app exits.**
  - The owner "should be prepared to respond to a request for the contents of the CLIPBOARD … (retaining the data to be able to return it)" ([ICCCM](https://www.x.org/releases/X11R7.6/doc/xorg-docs/specs/ICCCM/icccm.html)). The data lives only in the owning process.
  - arboard's README puts it this way: "X11 and Wayland put the responsibility for answering paste requests and serving data on the application which originally copied it" ([arboard README](https://github.com/1Password/arboard#clipboard-ownership)).
  - Consequences:
    - (a) Read the data **immediately** on each `SelectionNotify`. If you wait, the owner may be gone. `SelectionClientClose` / `SelectionWindowDestroy` tell you when that happened.
    - (b) When you paste back, *your* process becomes the owner and must keep serving requests.
  - The freedesktop ClipboardManager spec (`CLIPBOARD_MANAGER` / `SAVE_TARGETS`) lives at <https://www.freedesktop.org/wiki/ClipboardManager/>, but the wiki blocked automated fetches, so its details are **(unverified)** here.
- **Size limits on transfers.** The freedesktop Clipboard Extensions spec defines `_NET_MAX_SELECTION_SIZE`, which lets "xclipboard-like tools" cap a `MULTIPLE` request so they avoid "unwanted large transfers" ([clipboard-extensions spec](https://specifications.freedesktop.org/clipboard-extensions/latest/)). Few apps implement it, so enforce your own cap after reading **(inference)**.

### 1.2 Linux Wayland: the hard part

**Core protocol: focus-only.**
- `wl_data_device.selection` "is sent to a client immediately before receiving keyboard focus and when a new selection is set while the client has keyboard focus", and the offer is valid "until the client loses keyboard focus" ([wayland.xml](https://gitlab.freedesktop.org/wayland/wayland/-/blob/main/protocol/wayland.xml), read from `/usr/share/wayland/wayland.xml`).
- A background tray app therefore never sees copies made in other apps.

**Data-control protocols (privileged clipboard-manager API).**
- `ext-data-control-v1` (staging) "allows a privileged client to control data devices. In particular, the client will be able to manage the current selection and take the role of a clipboard manager." It is still "in the testing phase" ([ext-data-control-v1.xml](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/ext-data-control/ext-data-control-v1.xml); read from the local `wayland-protocols 1.45` copy).
- `wlr-data-control-unstable-v1` is marked deprecated: "For clipboard management, use the ext-data-control-v1 protocol". PRIMARY support arrived in its v2 ([wayland.app wlr](https://wayland.app/protocols/wlr-data-control-unstable-v1)).
- Compositor support according to wayland.app's tables ([ext](https://wayland.app/protocols/ext-data-control-v1), [wlr](https://wayland.app/protocols/wlr-data-control-unstable-v1)):

| Compositor (version wayland.app tested) | ext-data-control-v1 | wlr-data-control |
|---|---|---|
| **Mutter / GNOME 51** | **no** | **no** |
| KWin 6.7 | yes | no |
| Sway 1.11, Hyprland 0.52.1, COSMIC 1.0.0~beta.8, niri, labwc, river, Jay | yes | yes (v2) |
| Wayfire 0.9.0 | no | yes (v2) |
| Weston 15, Muffin (Cinnamon) 6.7.4 | no | no |

- **Mutter status.**
  - The request for wlr-data-control is [mutter#524](https://gitlab.gnome.org/GNOME/mutter/-/issues/524). The fetched page didn't show its state or any developer reply, so the issue's outcome is **(unverified)**.
  - The source itself is unambiguous. mutter `main` (51.1) `src/wayland/` contains `meta-wayland-data-device*.c` and the primary-selection files, but no data-control implementation ([tree](https://github.com/GNOME/mutter/tree/main/src/wayland)).
  - **(local check)** Ubuntu 24.04's `libmutter-14.so.0` exports `zwp_primary_selection_*` but no `*data_control*` symbols.

**What Mutter does have, and how to reach it.**
- **Internal `MetaSelection` with an `owner-changed` signal.** This is reachable only from inside the gnome-shell process, so only from a Shell extension.
  - Clipboard Indicator connects to `global.display.get_selection()` and `'owner-changed'` ([extension.js](https://github.com/Tudmotu/gnome-shell-extension-clipboard-indicator/blob/master/extension.js)).
  - GPaste's in-Shell mode does the same. Its source comment says this backend "talks to mutter's server-side selection tracker directly, which is only reachable from inside the gnome-shell process" ([gpaste-clipboard-meta.c](https://github.com/Keruspe/GPaste/blob/master/src/libgpaste/gpaste-daemon/gpaste-clipboard-meta.c)).
- **A private D-Bus API, `org.gnome.Mutter.Clipboard`.** It offers `Enable`, `SetSelection`, `SelectionRead` and `SelectionWrite`, plus a `SelectionOwnerChanged` signal ([interface XML](https://github.com/GNOME/mutter/blob/main/data/dbus-interfaces/org.gnome.Mutter.Clipboard.xml)).
  - Mutter wires it into RemoteDesktop sessions ([meta-remote-desktop-session.c](https://github.com/GNOME/mutter/blob/main/src/backends/meta-remote-desktop-session.c)) and checks callers with `MetaDbusAccessChecker` ([meta-dbus-access-checker.c](https://github.com/GNOME/mutter/blob/main/src/backends/meta-dbus-access-checker.c)).
  - Treat it as reachable only through the portal **(inference)**. I didn't trace which senders are allowed.
- **A built-in "clipboard manager" that keeps data after the owner exits.** When the CLIPBOARD owner goes away, Mutter re-serves **one** saved MIME type: the best match from a list of images plus `text/plain`/`text/plain;charset=utf-8`, capped at `MAX_TEXT_SIZE` 4 MB and `MAX_IMAGE_SIZE` 200 MB ([meta-clipboard-manager.c](https://github.com/GNOME/mutter/blob/main/src/core/meta-clipboard-manager.c)). Rich formats such as HTML are lost when the source app quits. A history app is still needed.

**XDG Clipboard portal: not a clipboard-manager API.**
- It says: "This portal does not create its own sessions." It only adds clipboard access to RemoteDesktop and InputCapture sessions. `RequestClipboard()` must be called before the session starts, and you then get `SelectionOwnerChanged`, `SelectionRead` and related calls ([Clipboard portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Clipboard.html)).
- With InputCapture the clipboard is available "only when the session is active" ([InputCapture portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html)), so it's useless for this.
- With RemoteDesktop, `Start()` "presents a dialog to the user". `persist_mode` 2 ("until explicitly revoked") plus a single-use `restore_token` (both added in RemoteDesktop v2) can restore later sessions ([RemoteDesktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)).
- xdg-desktop-portal-gnome implemented the Clipboard portal and "restoration of remote desktop sessions" in 45.beta ([NEWS](https://github.com/GNOME/xdg-desktop-portal-gnome/blob/main/NEWS)). **(local check)** Ubuntu 24.04's `gnome.portal` lists `Clipboard` and `RemoteDesktop`.
- **(inference)** A RemoteDesktop session with keyboard access + clipboard would technically give you monitoring *and* Ctrl+V injection on stock GNOME without an extension. The cost is a "remote control" consent dialog and probably a persistent screen-sharing/remote indicator **(unverified)**. That is a poor fit for an always-on clipboard app, but it's a usable opt-in fallback.

**XWayland route: works on GNOME without an extension.**
- Mutter claims every non-X11 selection on its own Xwayland window so X11 clients can read it. The code comment reads "If the owner is non-X11, claim the selection on our selection window, so X11 apps can interface with it". It calls `XSetSelectionOwner` on every `owner-changed`, with no focus check ([meta-x11-selection.c](https://github.com/GNOME/mutter/blob/main/src/x11/meta-x11-selection.c)).
- An X11 client listening with XFixes on the Xwayland display therefore sees every Wayland copy **(inference from source; worth a quick test on 26.04)**.
- Two shipping managers rely on this:
  - The GPaste daemon calls `gdk_set_allowed_backends ("x11")` with the comment "GTK doesn't support global clipboard events on wayland" ([gpaste-daemon.c](https://github.com/Keruspe/GPaste/blob/master/src/daemon/gpaste-daemon.c)).
  - CopyQ documents `QT_QPA_PLATFORM=xcb` to "force CopyQ to run under XWayland", noting it "can cause clipboard monitoring to fail when the main window is closed" ([CopyQ known issues](https://copyq.readthedocs.io/en/latest/known-issues.html)).
- In a Tauri app you don't need to move the whole GTK UI to X11. Run an `x11rb` XFixes connection to `$DISPLAY` from Rust next to the native-Wayland window **(inference)**.
- Caveats:
  - It requires Xwayland, which Ubuntu still ships ("You can still run applications developed for X.org through the XWayland compatibility layer", [26.04 notes](https://documentation.ubuntu.com/release-notes/26.04/summary-for-lts-users/)).
  - Data passes through Mutter's X11 bridge, so check that format fidelity (HTML, images) holds **(unverified)**.

**Ubuntu version map**

| Ubuntu | GNOME | GNOME X11 session | GlobalShortcuts portal |
|---|---|---|---|
| 24.04 LTS | 46 **(local check)** | available | no (portal-gnome 46.2's `gnome.portal` has no GlobalShortcuts, **(local check)**) |
| 25.04 | 48 ([notes](https://documentation.ubuntu.com/release-notes/25.04/)) | available | yes: backend added in portal-gnome 48.rc ([NEWS](https://github.com/GNOME/xdg-desktop-portal-gnome/blob/main/NEWS)); "fully supported by GNOME 48" ([GNOME 48 dev notes](https://release.gnome.org/48/developers/)) |
| 25.10 | 49 | **removed**: "GNOME Shell can no longer run as an X.org session" ([notes](https://documentation.ubuntu.com/release-notes/25.10/)) | yes |
| 26.04 LTS | 50 | removed; Wayland only ([notes](https://documentation.ubuntu.com/release-notes/26.04/summary-for-lts-users/)) | yes ("Apps can now set up system-wide keyboard shortcuts") |

None of these Mutter versions has data-control. So on stock Ubuntu (any version, Wayland session) the extension or the XWayland route is mandatory.

### 1.3 macOS

- **No notification; poll `changeCount`.** "The change count subsequently increments each time the pasteboard ownership changes" ([changeCount](https://developer.apple.com/documentation/appkit/nspasteboard/changecount)). Maccy polls with `Timer.scheduledTimer` at a default `clipboardCheckInterval` of 0.5 s ([Clipboard.swift](https://github.com/p0deje/Maccy/blob/master/Maccy/Clipboard.swift), [Defaults](https://github.com/p0deje/Maccy/blob/master/Maccy/Extensions/Defaults.Keys%2BNames.swift)). Clipy polls every 500 ms ([ClipService.swift](https://github.com/Clipy/Clipy/blob/develop/Clipy/Sources/Services/ClipService.swift)). `clipboard-master` does the same on macOS ([README](https://github.com/DoumanAsh/clipboard-master)).
- **Pasteboard privacy (macOS 15.4+).**
  - `NSPasteboard.accessBehavior` (macOS 15.4) is read-only: "The current pasteboard access behavior. The user can customize this behavior per-app in System Settings for any app that has triggered a pasteboard access alert in the past" ([accessBehavior](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-86972)).
  - The `AccessBehavior` enum has `alwaysAllow`, `alwaysDeny`, `ask` and `default` ([enum](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum)).
    - `.default`: "The default behavior for the General pasteboard is to ask upon programmatic access… Once programmatic pasteboard access triggers the first pasteboard access alert, the state automatically changes to `.ask`" ([default](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum/default)).
    - `.ask` and `.alwaysDeny` both still allow "access that is both user originated and paste related" without notification ([ask](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum/ask), [alwaysDeny](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum/alwaysdeny)).
    - `.alwaysAllow`: "allow all pasteboard access, without notifying the user" ([alwaysAllow](https://developer.apple.com/documentation/appkit/nspasteboard/accessbehavior-swift.enum/alwaysallow)).
  - Detection without reading: `detectedPatterns(for:)` "doesn't allow the app to access the item's contents. As a result, the system doesn't notify the person". `detectedValues(for:)` does read, and the system then "informs the person" and throws if denied ([detectedPatterns](https://developer.apple.com/documentation/appkit/nspasteboard/detectedpatterns(for:)), [detectedValues](https://developer.apple.com/documentation/appkit/nspasteboard/detectedvalues(for:))).
  - The documented API has **no** way to request "always allow". The app can only read `accessBehavior` and guide the user to System Settings.
  - Reading `changeCount` or `types` without reading contents probably doesn't trigger the alert, but Apple doesn't say so **(unverified)**.
  - The macOS 27.2 notes say "The `EnablePasteboardPrivacyDeveloperPreview` user default has been removed" ([27.2 notes](https://developer.apple.com/documentation/macos-release-notes/macos-27_2-release-notes)). I found no pasteboard entry in the 15.4, 26 or 27.0 release notes, so whether enforcement is now on by default is **(unverified)**.
- **Skip conventions ([nspasteboard.org](http://nspasteboard.org/)):**
  - `org.nspasteboard.TransientType`: "should not be recorded or displayed in a pasteboard history".
  - `org.nspasteboard.ConcealedType`: confidential. Obfuscate it on screen, and don't save it (or encrypt it if you do).
  - `org.nspasteboard.AutoGeneratedType`: "probably should not be recorded".
  - `org.nspasteboard.source`: the source bundle ID, for display only.
  - Maccy defines all of these types ([NSPasteboard.PasteboardType+Types.swift](https://github.com/p0deje/Maccy/blob/master/Maccy/Extensions/NSPasteboard.PasteboardType%2BTypes.swift)). arboard's `exclude_from_history` on macOS sets `org.nspasteboard.ConcealedType` ([osx.rs](https://github.com/1Password/arboard/blob/master/src/platform/osx.rs)).

### 1.4 Windows

- "There are three ways of monitoring changes to the clipboard… New programs should use clipboard format listeners or the clipboard sequence number." `AddClipboardFormatListener` posts `WM_CLIPBOARDUPDATE`, and it is "recommended over creating a clipboard viewer window". `GetClipboardSequenceNumber` "is a not a notification method and should not be used in a polling loop" ([Using the Clipboard](https://learn.microsoft.com/en-us/windows/win32/dataxchg/using-the-clipboard)).
- **Exclusion formats** ([Clipboard Formats → Cloud Clipboard and Clipboard History Formats](https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats)):
  - `ExcludeClipboardContentFromMonitorProcessing`: "Place any data on the clipboard in this format to prevent all clipboard formats being included in the clipboard history or synchronized".
  - `CanIncludeInClipboardHistory`: a DWORD 0 excludes the item from history, and 1 explicitly includes it.
  - `CanUploadToCloudClipboard`: the same idea for cloud sync.
  - All three are registered with `RegisterClipboardFormat`. Microsoft frames them for Windows' own history. A third-party manager should honor them by convention. Ditto skips on `ExcludeClipboardContentFromMonitorProcessing`, on `CanIncludeInClipboardHistory == 0`, and on its legacy `"Clipboard Viewer Ignore"` format ([Ditto ClipboardViewer.cpp](https://github.com/sabrogden/Ditto/blob/master/src/ClipboardViewer.cpp)).

---

## 2. Paste-back and global hotkey

| Platform | Global hotkey | Paste-back (synthesize Ctrl/Cmd+V) |
|---|---|---|
| Windows | `RegisterHotKey`. Win-key combos "are reserved for use by the operating system", and registration "typically… fails if the keystrokes… have already been registered" ([RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)). The Tauri plugin works here. | `SendInput`. It "is subject to UIPI. Applications are permitted to inject input only into applications that are at an equal or lesser integrity level", and fails silently ([SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)). Pasting into elevated apps won't work unless you are elevated too. |
| macOS | `global-hotkey` uses Carbon `RegisterEventHotKey` ([macos/mod.rs](https://github.com/tauri-apps/global-hotkey/blob/dev/src/platform_impl/macos/mod.rs)). Whether that needs any TCC permission is **(unverified)**; Maccy's FAQ only lists Accessibility for *pasting*. | `CGEvent(keyboardEventSource:…)` + `post(tap:)`, as Maccy ([Clipboard.swift](https://github.com/p0deje/Maccy/blob/master/Maccy/Clipboard.swift)) and Clipy ([PasteService.swift](https://github.com/Clipy/Clipy/blob/develop/Clipy/Sources/Services/PasteService.swift)) do. Maccy's FAQ: paste needs the app "added to System Settings → Privacy & Security → Accessibility" ([README](https://github.com/p0deje/Maccy#why-doesnt-it-paste-when-i-select-an-item-in-history)). Check with `AXIsProcessTrustedWithOptions` ([doc](https://developer.apple.com/documentation/applicationservices/1459186-axisprocesstrustedwithoptions)) or `CGPreflightPostEventAccess`/`CGRequestPostEventAccess` (macOS 10.15+, [doc](https://developer.apple.com/documentation/coregraphics/cgpreflightposteventaccess())). |
| Linux X11 | `XGrabKey` via `global-hotkey`'s X11 backend ([global-hotkey](https://github.com/tauri-apps/global-hotkey)). | XTest fake key events **(standard technique; not re-verified here)**. |
| Linux Wayland | **GlobalShortcuts portal** (v2): `CreateSession`, `BindShortcuts`, `ListShortcuts`, `ConfigureShortcuts`, and an `Activated` signal with an `activation_token`. `preferred_trigger` is only a preference; the response returns a `trigger_description`, so the user/portal decides ([GlobalShortcuts portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html)). Rust: `ashpd::desktop::global_shortcuts` ([docs.rs](https://docs.rs/ashpd/latest/ashpd/desktop/global_shortcuts/index.html)). GNOME 48+ only (see version map). For unsandboxed `.deb`/AppImage apps, xdg-desktop-portal ≥ 1.19.4/1.20 has a host app registry so a host app can "register themselves… to use a proper app id" ([NEWS](https://github.com/flatpak/xdg-desktop-portal/blob/main/NEWS.md), [Registry](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.host.portal.Registry.html)). Ubuntu 24.04 ships xdg-desktop-portal 1.18.4 **(local check)**, so it has neither GlobalShortcuts nor the registry. GPaste and CopyQ both use the portal for shortcuts ([GPaste README](https://github.com/Keruspe/GPaste), [CopyQ](https://copyq.readthedocs.io/en/latest/known-issues.html)). | Without a privileged path you can't inject. Options: (1) RemoteDesktop portal `NotifyKeyboardKeysym`/EIS after a consent dialog ([RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)); (2) let the GNOME extension do it **(unverified)**; (3) `/dev/uinput` tools (root/udev setup; not appropriate for a public app **(inference)**); (4) XTest from an XWayland client. Mutter passes `-enable-ei-portal` to Xwayland when configured ([meta-xwayland.c](https://github.com/GNOME/mutter/blob/main/src/wayland/meta-xwayland.c)), which would route XTEST through the portal dialog. Whether stock GNOME enables it, and how XTEST behaves when a Wayland window has focus, is **(unverified)**. **Default: copy the item, close the popup, and let the user press Ctrl+V.** |

Other Wayland UI notes:

- `xdg_toplevel` has no request to set a window position; its requests are `set_parent`, `set_title`, `move` (interactive), `resize`, `set_maximized`, and so on ([xdg-shell.xml](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/stable/xdg-shell/xdg-shell.xml), read locally). A "popup at the cursor" UI won't work on Wayland; plan for a compositor-placed window **(inference)**.
- Use the `activation_token` from the portal's `Activated` signal to raise your window, or GNOME's focus-stealing prevention may leave it behind other windows **(inference)**.
- **GNOME 46/47 fallback:** have the user add a custom keyboard shortcut that runs `yourapp --toggle`. `tauri-plugin-single-instance` uses `zbus` (D-Bus) on Linux to forward the second launch to the running app ([Cargo.toml](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/single-instance/Cargo.toml)).

---

## 3. Formats, sizes, storage

- **Linux:** MIME types (`text/plain;charset=utf-8`, `text/html`, `image/png`, `text/uri-list` for files). Mutter's own manager and GPaste's Meta backend use exactly these ([meta-clipboard-manager.c](https://github.com/GNOME/mutter/blob/main/src/core/meta-clipboard-manager.c), [gpaste-clipboard-meta.c](https://github.com/Keruspe/GPaste/blob/master/src/libgpaste/gpaste-daemon/gpaste-clipboard-meta.c)).
  - Apps may also offer private targets. Klipper ignores items carrying `application/x-kde-syncselection`, and it treats `x-kde-passwordManagerHint == "secret"` as a password ([systemclipboard.cpp](https://github.com/KDE/plasma-workspace/blob/master/klipper/systemclipboard.cpp), [historymodel.cpp](https://github.com/KDE/plasma-workspace/blob/master/klipper/historymodel.cpp)).
  - GPaste honors the same KDE hint ([GPaste README](https://github.com/Keruspe/GPaste)), and arboard sets it in `exclude_from_history` (`KDE_EXCLUSION_MIME`, [linux/mod.rs](https://github.com/1Password/arboard/blob/master/src/platform/linux/mod.rs)).
- **macOS:** `NSPasteboard.PasteboardType` includes `string`, `rtf`, `rtfd`, `html`, `png`, `tiff`, `pdf`, `fileURL`, `URL`, `color` and more ([PasteboardType](https://developer.apple.com/documentation/appkit/nspasteboard/pasteboardtype)). One pasteboard can hold multiple items (`pasteboardItems`). Content copied with `.currentHostOnly` is "available only on the current device" ([currentHostOnly](https://developer.apple.com/documentation/appkit/nspasteboard/contentsoptions/currenthostonly)). Respect it by not syncing it, if you ever add sync **(inference)**.
- **Windows:**
  - Standard formats include `CF_UNICODETEXT`, `CF_DIB`/`CF_DIBV5` (bitmap headers plus raw bits) and `CF_HDROP` (file list via `DragQueryFile`) ([Standard Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/dataxchg/standard-clipboard-formats)).
  - Rich formats are registered: `"HTML Format"` (CF_HTML) is "always using UTF-8" with `Version`/`StartHTML`/`EndHTML`/`StartFragment`/`EndFragment` byte offsets ([HTML Clipboard Format](https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format)), and RTF is registered as well ([Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats)).
  - The system synthesizes conversions such as `CF_DIB ↔ CF_BITMAP` and `CF_TEXT ↔ CF_UNICODETEXT` (same page).
  - Delayed rendering exists: `SetClipboardData(fmt, NULL)` + `WM_RENDERFORMAT` ([Using the Clipboard](https://learn.microsoft.com/en-us/windows/win32/dataxchg/using-the-clipboard)). A monitor reading every format forces every source to render everything, so read only the formats you store **(inference)**.
- **Sizes and storage (inference unless cited):**
  - Bitmaps are uncompressed on Windows (DIB). A 3840×2160 32-bit screenshot is about 33 MB raw, so re-encode to PNG (or WebP) before storing.
  - Cap text per item (Mutter caps at 4 MB) and images per item, and keep a total-size budget for the history.
  - Store blobs out of the main row (a separate table or files by content hash). GPaste de-duplicates images "by checksum, kept as files or as database blobs" ([GPaste README](https://github.com/Keruspe/GPaste)).
  - For files, store the path or URI list, not file contents.
  - Keep the full multi-format set for each item so paste-back restores the rich version, with plain text alongside.

---

## 4. Tauri 2 specifics and crates

**Official plugins** (read at plugins-workspace `v2`, HEAD 2026-11-10):

- **clipboard-manager 2.5.0:**
  - Depends on `arboard 3.6` with `features = ["wayland-data-control"]` ([Cargo.toml](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/clipboard-manager/Cargo.toml)).
  - The API is read/write only and has **no watch/listen API** ([desktop.rs](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/clipboard-manager/src/desktop.rs)).
  - `write_secret` calls arboard's `exclude_from_monitoring` on Windows and `exclude_from_history` elsewhere ([secret.rs](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/clipboard-manager/src/secret.rs)).
  - Fine for paste-back of text/HTML/image on Win/macOS/X11. Not enough for monitoring, files or multi-format items.
- **global-shortcut 2.4.1:** depends on `global-hotkey 0.8` ([Cargo.toml](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/global-shortcut/Cargo.toml)). `global-hotkey` has backends for `macos`, `windows` and `x11` only, and the README says "Linux (X11 Only)" ([repo](https://github.com/tauri-apps/global-hotkey)). It has **no Wayland/portal support**; use `ashpd` yourself.
- **tray:** Tauri docs say tray click events on Linux are "Unsupported. The event is not emitted even though the icon is shown". The menu still works on right-click ([System Tray](https://v2.tauri.app/learn/system-tray/)). On GNOME the tray needs the AppIndicator extension; Ubuntu ships `ubuntu-appindicators@ubuntu.com` **(local check)**. Plan the tray as menu-only on Linux.
- **autostart:** wraps `auto-launch 0.6`. On macOS it defaults to `MacosLauncher::LaunchAgent` (a plist in `~/Library/LaunchAgents`), with an `AppleScript` alternative ([lib.rs](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/autostart/src/lib.rs)). For the Mac App Store, guideline 2.4.5(iii) forbids auto-launch "without consent" ([App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)). Whether a LaunchAgent plist is allowed in the sandbox at all is **(unverified)**; Apple's `SMAppService` is the usual route there.
- **single-instance:** uses `zbus` on Linux ([Cargo.toml](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/single-instance/Cargo.toml)). It is the basis for the `--toggle` CLI shortcut fallback.

**Crates** (versions and last-update dates from the crates.io API on 2026-11-12; repos checked out at HEAD):

| Crate | Latest / updated | Role | Wayland notes (from source) |
|---|---|---|---|
| [`arboard`](https://github.com/1Password/arboard) | 3.7.0 / 2026-10-15 (1Password) | read/write text, HTML, images; secret hints | The `wayland-data-control` feature (uses `wl-clipboard-rs`) is off by default. If init fails it logs "Falling back to the X11 clipboard protocol" ([linux/mod.rs](https://github.com/1Password/arboard/blob/master/src/platform/linux/mod.rs)). README: without the protocols it "won't function in a pure Wayland environment… recommended to enable XWayland", and sandboxes need the X11 socket "in addition" ([README](https://github.com/1Password/arboard)). No watch API. |
| [`clipboard-master`](https://github.com/DoumanAsh/clipboard-master) | 4.1.0 / 2026-10-23 | change monitoring | Windows: hidden window. macOS: `changeCount` polling. Linux: `x11rb` XFixes or `wl-clipboard-rs` ([README](https://github.com/DoumanAsh/clipboard-master), [Cargo.toml](https://github.com/DoumanAsh/clipboard-master/blob/master/Cargo.toml)). **Pitfalls in [linux/mod.rs](https://github.com/DoumanAsh/clipboard-master/blob/master/src/master/linux/mod.rs):** it checks a misspelled `WAYLAND_DISPALY`, then picks Wayland if `XDG_SESSION_TYPE=wayland`, with **no fallback to X11**. On GNOME Wayland it would therefore choose the data-control backend, which Mutter lacks **(inference)**. Use it on Win/macOS; write your own Linux selection logic. |
| [`wl-clipboard-rs`](https://github.com/YaLTeR/wl-clipboard-rs) | 0.9.5 / 2026-10-26 | Wayland data-control client | Supports both `ext-data-control` and `wlr-data-control` ([common.rs](https://github.com/YaLTeR/wl-clipboard-rs/blob/master/src/common.rs)). Has a `watch::Watcher` with `next_event()` and a `CancelHandle` ([README](https://github.com/YaLTeR/wl-clipboard-rs)). Useless on GNOME. |
| [`x11rb`](https://github.com/psychon/x11rb) | 0.14.0 / 2026-07-16 | X11/XWayland protocol incl. `xfixes` | Used for the XFixes backend (and on GNOME via Xwayland). |
| [`objc2-app-kit`](https://github.com/madsmtm/objc2) | 0.3.2 / 2025-10-04 | NSPasteboard bindings | Used by arboard and clipboard-master. Check that it exposes the 15.4 `accessBehavior`/detect APIs **(unverified)**. |
| [`windows`](https://github.com/microsoft/windows-rs) | 0.62.2 / 2025-10-06 (Microsoft) | Win32 (`AddClipboardFormatListener`, `SendInput`, DPAPI) | n/a |
| [`ashpd`](https://github.com/bilelmoussaoui/ashpd) | 0.14.0 / 2026-11-05 | XDG portals (GlobalShortcuts, RemoteDesktop, Clipboard) | The Wayland hotkey path. |
| [`keyring`](https://github.com/open-source-cooperative/keyring-rs) | 4.2.0 / 2026-08-29 | OS secret store for the history key | n/a |
| [`global-hotkey`](https://github.com/tauri-apps/global-hotkey) | 0.10.0 / 2026-11-09 | hotkeys | X11 only on Linux. |

---

## 5. How existing open-source managers do it

- **CopyQ** (Qt; [repo](https://github.com/hluk/CopyQ)):
  - Wayland: its own `zwlr_data_control` client ([waylandclipboard.cpp](https://github.com/hluk/CopyQ/blob/master/src/platform/x11/systemclipboard/waylandclipboard.cpp)).
  - GNOME: a **bundled Shell extension** that owns `com.github.hluk.copyq.GnomeClipboard` on the session bus. It exports `RegisterClipboardClient`/`GetClipboardData`/`SetClipboardData`/`GetClipboardFormats` and listens to `MetaSelection` `owner-changed` (`shell-version` 45–50) ([extension.js](https://github.com/hluk/CopyQ/blob/master/shared/gnome-extension/extension.js), [metadata.json](https://github.com/hluk/CopyQ/blob/master/shared/gnome-extension/metadata.json)).
  - Docs: native when "the compositor supports the required Wayland protocol", a GNOME extension otherwise, the `xcb` fallback, and shortcuts via the portal ([known issues](https://copyq.readthedocs.io/en/latest/known-issues.html)).
  - **This is the closest blueprint for your Linux design.**
- **GPaste** (GNOME; [repo](https://github.com/Keruspe/GPaste)): the daemon forces GDK's X11 backend because "GTK doesn't support global clipboard events on wayland" ([gpaste-daemon.c](https://github.com/Keruspe/GPaste/blob/master/src/daemon/gpaste-daemon.c)). It also has an experimental mode running inside gnome-shell on `MetaSelection` ([gpaste-clipboard-meta.c](https://github.com/Keruspe/GPaste/blob/master/src/libgpaste/gpaste-daemon/gpaste-clipboard-meta.c)). Other features: portal shortcuts, optional libsodium encryption at rest, the KDE password hint, and SQLite storage ([README](https://github.com/Keruspe/GPaste)).
- **Clipboard Indicator** (pure GNOME Shell extension, shell 47–49+; [extension.js](https://github.com/Tudmotu/gnome-shell-extension-clipboard-indicator/blob/master/extension.js)): `global.display.get_selection().connect('owner-changed', …)` plus `St.Clipboard` for read/write, with a "private mode" toggle.
- **Klipper** (KDE; [plasma-workspace/klipper](https://github.com/KDE/plasma-workspace/tree/master/klipper)): `KSystemClipboard` (KWin supports ext-data-control). Honors `x-kde-passwordManagerHint`.
- **Maccy** (macOS; [repo](https://github.com/p0deje/Maccy)):
  - 0.5 s `changeCount` polling.
  - Skips concealed/transient/auto-generated types.
  - `CGEvent` Cmd+V paste with Accessibility.
  - Its entitlements enable `com.apple.security.app-sandbox` ([Maccy.entitlements](https://github.com/p0deje/Maccy/blob/master/Maccy/Maccy.entitlements)), which suggests polling and paste-back work under App Sandbox **(inference)**.
- **Clipy** (macOS; [repo](https://github.com/Clipy/Clipy), not archived): 500 ms polling, an option to ignore concealed types, and `CGEvent` paste ([ClipService.swift](https://github.com/Clipy/Clipy/blob/develop/Clipy/Sources/Services/ClipService.swift), [PasteService.swift](https://github.com/Clipy/Clipy/blob/develop/Clipy/Sources/Services/PasteService.swift)).
- **Ditto** (Windows; [ClipboardViewer.cpp](https://github.com/sabrogden/Ditto/blob/master/src/ClipboardViewer.cpp)): `AddClipboardFormatListener` (loaded with `GetProcAddress`), falling back to `SetClipboardViewer`. Honors the three exclusion conventions listed in §1.4.

---

## 6. Distribution

### Ubuntu / Linux

- Tauri documents bundles for Debian, RPM, AppImage, Snap, Flatpak and AUR ([Tauri distribute](https://v2.tauri.app/distribute/)).
- **.deb / AppImage (unsandboxed):** every backend works: X11 socket, session D-Bus to the extension, and portals.
  - The GNOME extension has to be installed separately, either from extensions.gnome.org or bundled in the .deb under `/usr/share/gnome-shell/extensions/` **(inference)**.
  - Extensions.gnome.org forbids bundling binaries in the extension and requires declaring clipboard access ([review guidelines](https://gjs.guide/extensions/review-guidelines/review-guidelines.html)).
  - **Recommended first target.**
- **Snap (strict):**
  - The `x11` interface's base declaration only denies auto-connection `on-classic: false`, which implies it auto-connects on classic Ubuntu desktops ([snapd x11.go](https://github.com/canonical/snapd/blob/master/interfaces/builtin/x11.go)) **(inference from the declaration)**. So the XWayland/XFixes route should work.
  - Talking to a Shell extension's D-Bus name needs a `dbus` plug or an AppArmor rule. Whether a store snap can get that is **(unverified)**.
  - Portals work via the `desktop` interface **(unverified detail)**.
- **Flatpak / Flathub:**
  - `--socket=x11` or `fallback-x11` gives Xwayland access ([Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)).
  - Talking to the extension needs `--talk-name=<your bus name>`. Flatpak says to use "the minimum required talk-name permissions", and full session-bus access "must be avoided" (same page).
  - Flathub: "Static permissions must be kept to an absolute minimum", portals are mandatory "when a suitable XDG portal exists", and apps that "rely on host components… for core functionality will not be accepted" ([Flathub requirements](https://docs.flathub.org/docs/for-app-authors/requirements)). A Flathub build that depends on a host Shell extension is therefore at risk of rejection. The X11 route is the more defensible Flathub story **(inference)**.

### macOS

- **Outside the App Store (recommended):**
  - Use a Developer ID certificate, the Hardened Runtime, a secure timestamp, `notarytool`, and stapling.
  - "Beginning in macOS 10.15, all software built after June 1, 2019, and distributed with Developer ID must be notarized." The App Store doesn't require separate notarization ([Notarizing macOS software](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution)).
  - Tauri: "code signing and notarization… are required for distribution outside the App Store" ([Tauri distribute](https://v2.tauri.app/distribute/)).
- **Mac App Store:**
  - Must be sandboxed (2.4.5(i)). No auto-launch "without consent" (2.4.5(iii)). Updates only via the store (2.4.5(vii)) ([App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)).
  - Tauri needs the App Sandbox entitlement, a Mac App Store Connect provisioning profile, and a pkg signed with a Mac Installer Distribution certificate ([Tauri App Store](https://v2.tauri.app/distribute/app-store/)).
  - Guideline 2.5.14 requires "explicit user consent and… a clear visual and/or audible indication when recording, logging, or otherwise making a record of user activity" (same guidelines page). A clipboard history is arguably that, so show clear onboarding and a tray indicator.
  - Whether review accepts Accessibility-based auto-paste in a sandboxed store app is **(unverified)**.
  - You'd lose the Tauri updater plugin in the store build.

### Windows

- Tauri "only generates EXE and MSI installers". Store listing works by linking a signed, silent-installable, offline-WebView2 installer ([Tauri Microsoft Store](https://v2.tauri.app/distribute/microsoft-store/)).
- Signing ([Code signing options](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options)):
  - MSIX through the Store is re-signed by Microsoft for free. MSI/EXE through the Store must be Authenticode-signed with a Trusted-Root CA certificate.
  - Outside the Store: Azure Artifact Signing costs about $9.99/month. Individuals can use it only in the USA/Canada; organizations in the USA, Canada, EU and UK.
  - OV certificates cost $150–300/year and need an HSM.
  - EV "no longer instant bypass" of SmartScreen since 2024.
  - SignPath Foundation offers free signing for qualifying OSS projects.

---

## 7. Security and privacy

- **Skip secrets at capture time.**
  - macOS: `TransientType`, `ConcealedType`, `AutoGeneratedType` ([nspasteboard.org](http://nspasteboard.org/)).
  - Windows: `ExcludeClipboardContentFromMonitorProcessing`, and `CanIncludeInClipboardHistory = 0` ([Clipboard Formats](https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats)).
  - Linux: `x-kde-passwordManagerHint: secret` ([Klipper](https://github.com/KDE/plasma-workspace/blob/master/klipper/historymodel.cpp)).
  - These are exactly the hints arboard/Tauri's `write_secret` emit ([secret.rs](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/clipboard-manager/src/secret.rs)), so password managers built on them will be skipped.
  - Also add: a per-app ignore list (using `org.nspasteboard.source` on macOS, the foreground process on Windows/X11), a "pause / ignore next copy" toggle (Maccy has both, [README](https://github.com/p0deje/Maccy#ignore-copied-items)), and optional regex filters (card numbers, tokens) **(inference)**.
- **Encrypt at rest.**
  - Generate a random data key and keep it in the OS store:
    - macOS Keychain ("an encrypted database called a keychain", [Keychain services](https://developer.apple.com/documentation/security/keychain-services)).
    - Windows DPAPI `CryptProtectData`, where "only a user with the same logon credential… can decrypt" ([CryptProtectData](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)).
    - The Linux Secret Service ([spec](https://specifications.freedesktop.org/secret-service/latest/)).
    - The `keyring` crate abstracts all three.
  - Encrypt rows/blobs (or use SQLCipher). GPaste ships optional libsodium encryption with the passphrase optionally in the keyring ([README](https://github.com/Keruspe/GPaste)), which is a precedent.
  - Note that same-user malware can usually unlock the same key, so this protects backups and disk theft more than a live session **(inference)**.
- **Retention:**
  - Limits by count, age and total bytes. Pinned items are exempt (GPaste: "the size and memory limits give way instead" of dropping favourites, [README](https://github.com/Keruspe/GPaste)).
  - Concealed items auto-expire (GPaste password timeouts).
  - "Clear history" also clears the live clipboard if it came from us.
  - Never sync or upload by default, and never log content.
- **Bridge security (Linux):** a session-bus clipboard service, like CopyQ's extension, can be called by **any** unsandboxed process in the session. That's no worse than X11, where any client can read selections, but restrict methods to your registered client where you can **(inference)**.

---

## Capability matrix

| | Windows 10/11 | macOS 14–27 | Linux X11 (24.04 Xorg session; KDE/Xfce on X) | GNOME Wayland (Ubuntu 24.04–26.04) | KDE/wlroots/COSMIC Wayland |
|---|---|---|---|---|---|
| Change events | ✅ `WM_CLIPBOARDUPDATE` | ⚠️ poll `changeCount` | ✅ XFixes | ❌ native. ✅ via Shell extension; ✅ via XWayland XFixes **(inference)**; ⚠️ RemoteDesktop+Clipboard portal (dialog) | ✅ ext/wlr-data-control |
| Read contents in background | ✅ | ⚠️ 15.4+ access alert; user picks Always Allow | ✅ | ✅ via extension / XWayland | ✅ |
| PRIMARY selection | n/a | n/a | ✅ | ✅ via extension (`MetaSelectionType`) **(inference)** | ✅ (wlr v2 / ext) |
| Set clipboard for paste-back | ✅ | ✅ | ✅ (must keep serving) | ✅ (focus needed natively; via extension or XWayland otherwise) | ✅ data-control |
| Synthesize Ctrl/Cmd+V | ✅ `SendInput` (UIPI limits) | ✅ `CGEvent` + Accessibility | ✅ XTest | ❌ default. ⚠️ RemoteDesktop portal dialog / extension **(unverified)** | ⚠️ RemoteDesktop portal |
| Global hotkey | ✅ `RegisterHotKey` (not Win+V) | ✅ Carbon hotkey | ✅ `XGrabKey` | 24.04: ❌ (custom shortcut → CLI). 25.04+: ✅ GlobalShortcuts portal. Super+V taken by default | ✅ portal (KDE); others vary |
| Tauri plugins enough? | mostly (add listener) | mostly (add poller) | mostly (add XFixes) | ❌ custom backend + extension + ashpd | ❌ custom backend + ashpd |
| Sandboxed packaging | MSIX not generated by Tauri; Store via EXE/MSI link | App Store: sandbox OK for polling (Maccy) **(inference)**; auto-paste review **(unverified)** | Snap/Flatpak with x11 socket | Flatpak: host extension conflicts with Flathub policy | Flatpak: data-control works over the Wayland socket **(unverified)** |

---

## Open questions

1. **Measure the XWayland route on Ubuntu 26.04 (GNOME 50).** Does an `x11rb` XFixes client get `SelectionNotify` for every Wayland copy, including when no X11 window is mapped or focused? Do HTML and PNG targets come through intact? Source says yes for ownership mirroring ([meta-x11-selection.c](https://github.com/GNOME/mutter/blob/main/src/x11/meta-x11-selection.c)); behavior is untested.
2. **Does Xwayland start on demand** for a client that only opens a connection (no window), and does that keep Xwayland alive forever? **(unverified)**
3. **macOS enforcement.** On macOS 27.x with no defaults set, does a Developer-ID app reading `NSPasteboard.general` get the alert? Does reading `changeCount`/`types` alone trigger it? Which item reads count as "programmatic"?
4. **Mac App Store viability** of a sandboxed clipboard manager with Accessibility auto-paste and login-at-start (`SMAppService`).
5. **GNOME extension review.** Is a D-Bus bridge that serves clipboard data to the same project's app acceptable under "MUST NOT share clipboard data with a third-party without explicit user interaction"? Can the extension also synthesize Ctrl+V (Clutter virtual input device) without breaking the rules? **(unverified)**
6. **GlobalShortcuts for host (.deb/AppImage) apps on GNOME 48+:** does the GNOME backend need the host-app registry or a matching `.desktop` file to bind and persist shortcuts? What does the user see on first bind?
7. **RemoteDesktop-portal fallback UX on GNOME:** with `persist_mode=2`, does GNOME restore without a dialog after reboot? What indicator does it show while the session is active?
8. **Snap:** can a strict snap on the Snap Store talk to a Shell-extension D-Bus name, or must Snap builds rely on the x11 interface only?
9. **mutter#524 / data-control in Mutter:** confirm the current status and any GNOME position on ext-data-control (GitLab was bot-walled for this research).
10. **Tauri on Wayland window placement:** how does a Tauri/WebKitGTK popup behave on Mutter (always-on-top, centering, focus on hotkey with the activation token)? Prototype early.
11. **freedesktop ClipboardManager spec (`SAVE_TARGETS`)**: couldn't fetch it (bot wall). Decide whether to implement it as the X11 clipboard manager.

## Couldn't verify (summary)

- mutter#524 state and maintainers' reasoning. Mutter's allowed D-Bus senders for `org.gnome.Mutter.Clipboard`.
- Whether macOS pasteboard alerts are enforced by default on 26/27, and whether `changeCount` reads are exempt. App Store acceptance of auto-paste.
- Whether a GNOME extension can or may inject keystrokes. Whether stock GNOME enables Xwayland's EI portal for XTEST (only the snippet-level claim that libei XTEST arrived in Xwayland 23.2 was found, via a [mailing-list announcement](https://lists.freedesktop.org/archives/xorg/2023-July/061397.html) that returned 403 when fetched).
- Carbon `RegisterEventHotKey` permission requirements on current macOS.
- Snap `desktop`/`dbus` interface details. Flatpak data-control behavior. The freedesktop ClipboardManager wiki contents.
- `objc2-app-kit` coverage of the 15.4 `accessBehavior`/detect APIs.
