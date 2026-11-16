# Clipboard History

A desktop app that remembers what the user copied and lets them paste any of it again from a popup.

## Language

**Clip**:
One remembered copy: all its Formats, a preview, a content hash, source app, pinned flag and timestamps.
_Avoid_: Entry, item, snippet

**Format**:
One representation of a Clip's content, identified by MIME type (`text/plain`, `text/html`, `image/png`...). A Clip keeps every Format it was copied with.
_Avoid_: Type, flavor, target (X11 term)

**ClipEvent**:
What a Backend emits when the clipboard changes: the Formats plus source app and secret flag. The engine decides whether it becomes a new Clip, bumps an existing one, or is skipped.
_Avoid_: Change, notification

**Backend**:
The platform-specific implementation of watching and writing the clipboard (and, alongside it, pasting, shortcuts and focus tracking) for one OS or display server.
_Avoid_: Driver, provider

**Capabilities**:
What a Backend reports it can do (source app, auto-paste, files). The UI greys out anything missing with a one-line reason.
_Avoid_: Features, permissions

**Echo**:
The ClipEvent caused by the app's own write to the clipboard. It only bumps the existing Clip; it never creates a new one.
_Avoid_: Self-copy, loopback

**Pinned**:
A Clip the user keeps. Pinned Clips are never removed by Retention and do not count toward its limits.
_Avoid_: Starred, favourite

**Retention**:
The rule that, after each insert, removes unpinned Clips beyond the maximum count or older than the maximum age.
_Avoid_: Cleanup, expiry

**Force Super+V**:
An opt-in setting that takes the Super+V binding from the OS (GNOME's message tray, KDE's Klipper, Windows' Win+V) after saving the original.
_Avoid_: Override shortcut

**Reset to default**:
The Settings action that restores the OS binding saved before Force Super+V changed it, exactly as it was.
_Avoid_: Undo, revert
