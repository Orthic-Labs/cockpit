# Handoff: Send to Pulse inside ScrapeRight for iPhone

For the ScrapeRight chat. Written by the Pulse Mac chat on 2026-10-10 from Adrian's decisions (docs/decisions.md). RightKit owns the Send engine (`rightkit-send`); Pulse owns the protocol and the trust rules; ScrapeRight owns its app, its queue and CloudKit.

## Start here (cold chat)

- You are the ScrapeRight chat, working in the ScrapeRight checkout. Read that repo's `AGENTS.md` and `ios-native/README.md` first; they win over this file for anything about ScrapeRight itself.
- This file is the whole brief. Nothing else from the Pulse conversation is needed.
- Pulse is Adrian's notch app on the Mac and the Dell (repo `Orthic-Labs/pulse`). Its Send feature moves text, links, pictures and files between his computers. Today his phone uses the LocalSend app for this, which cannot prove who it is, so the computers treat it with limits. He wants the phone side inside ScrapeRight's iPhone app instead, as a fully trusted device.
- Do not edit the Pulse repo. Ask the Pulse Mac chat (session title "Pulse", working in the Pulse checkout) for anything on the Pulse side; it owns the protocol and the trust rules. Ask the RightKit lead chat for the library.
- The app's tabs and plus button are as Adrian described them (Queue, Library, Settings; a plus that adds photos and files). Check them against the code before designing.

## State on 2026-10-10

- Decided by Adrian: everything under "What Adrian wants".
- Not built: all of it. The Send engine exists only inside Pulse (`core/src/localsend/`); it is **not yet packaged** as a library for iOS. RightKit packages it as the `rightkit-send` crate (see below); ask the RightKit lead chat for the library and the Pulse chat for protocol questions.
- First step for you: read the ScrapeRight iOS code, then confirm the agreed points below and send the RightKit lead the function list you want from the library. Build the clipboard offer, the Process lane and the Receive tab's shell against a stub until the library arrives.

## What Adrian wants

1. **No Send tab.** Sending starts from content, wherever it enters the app:
   - the clipboard, when ScrapeRight opens (or returns to the foreground) and it holds something **new**; no paste step;
   - the existing plus button, once a photo or file is picked.
   In both cases the same choices appear: **Process**, and one **Send to <computer>** per Pulse computer found on the network (for example "Mac", "Dell").
2. **Process**: ScrapeRight handles it. A URL or text the phone can process is processed in the app. What the phone cannot process (YouTube, Instagram and similar) is queued through CloudKit and processed the next time ScrapeRight runs on the Mac.
3. **Send to <computer>**: the content lands on that computer's clipboard; files also land in its save folder.
4. Away from home no Pulse computer is found, so only Process shows. No error, no empty buttons.
5. **A Receive tab** beside Queue, Library and Settings: what the Mac and Dell send to the phone arrives here (text to copy, links to open, files and photos to save or share). While the tab's app is open the phone is visible to the computers as a device to send to.
6. This replaces LocalSend and "WhatsApp to myself" on his phone.

## iOS facts that shape the design

- Reading the clipboard shows the system "Allow Paste" prompt unless the user sets Settings > ScrapeRight > Paste from Other Apps > Allow. Detect first without reading: `UIPasteboard.general.hasURLs / hasStrings / hasImages` and `detectPatterns(for:)` raise no prompt. Show the offer from that, and read the content only when a button is tapped.
- "There is always something in the clipboard": offer only when `UIPasteboard.general.changeCount` differs from the count stored at the last offer or dismissal.
- Receiving from a computer works only while the app is open (iOS gives no background listener). A notification that asks to open the app is a later extra. A share-sheet action is also a later extra, not the main path.
- Discovery by multicast UDP needs the `com.apple.developer.networking.multicast` entitlement (Apple approval). Use Bonjour instead, which needs only `NSLocalNetworkUsageDescription` and `NSBonjourServices`. Pulse will advertise a Bonjour service for this (name to be agreed, see open points).

## What Pulse provides

- The Send engine: Rust, `core/src/localsend/` in Orthic-Labs/pulse (LocalSend protocol v2.2 over TLS, with Pulse's additions). To be packaged as a static library with a small C or UniFFI surface: identity (create and keep a certificate), discover, send(text | files, clipboard: true), receive.
- Trust: each device has its own certificate; the SHA-256 of it is its fingerprint. A sender that presents its certificate in the TLS handshake is proven. On the Mac and Dell, Adrian sets the phone to **Allow** once in the hub (Nearby > Devices); after that nothing is asked.
- Wire addition: `"pulse": {"clipboard": true}` in the prepare-upload request marks a paste, which the receiving computer puts on its clipboard ("Copied from <phone>").
- Received files on the computers are quarantined and never opened automatically.

## What ScrapeRight builds

- The clipboard and plus-button offer, and the Receive tab, described above.
- Keychain storage for the phone's certificate and key (the library hands over bytes; the app stores them).
- The Process lane and the CloudKit queue for items the phone cannot process.
- Hiding every "Send to" control when no Pulse computer is found.

## Agreed between Pulse and RightKit (2026-10-10), for ScrapeRight to confirm

1. **Library delivery:** RightKit extracts the engine into a crate, working name `rightkit-send`, and owns its publication and the iOS packaging (static library with a C surface, wrapped as `RightKitSend` in RightKitSwift). Pulse keeps authority over the wire extensions and the trust rules. ScrapeRight builds against a stub until the crate exists and sends RightKit the function list it wants. Adrian approved the move on 2026-10-10: Pulse moves onto the crate fully and deletes its own copy once the parity suite passes (every security rule from the reviews as a named test, run against both engines, on Pulse's Rust version).
2. **Bonjour:** service type `_pulse-send._tcp`, port from SRV. TXT keys: `fp` (64-hex lowercase SHA-256 of the device certificate), `alias` (display name, max 80 characters), `v` ("2.2"), `type` (desktop, mobile, web, headless, server), `model` (optional), `px` ("1": understands the `pulse` extensions and presents its certificate as a TLS client certificate). TXT is an announcement only; identity is proven by the certificate in the handshake. The iOS build compiles without the multicast path.
3. **Identity:** one stored certificate per install, supplied by the app (Keychain on iOS).

4. **Incoming offers:** no text from a sender is shown before the offer is accepted. A device on the phone's trusted list is accepted without asking only when it proves itself with its certificate in the handshake; a matching name or fingerprint alone still asks.

## Receive tab: no history (Adrian, 2026-10-10)

- **Text and links are a scratchpad.** Removed as soon as they are used (copied, opened, sent to Process) or dismissed; not kept across app launches. A received link shows Open and Process; either one removes it.
- **Pictures go to Photos.** Ask for add-only Photos access once (`NSPhotoLibraryAddUsageDescription`), then save without asking.
- **Files are saved to ScrapeRight's own "Received" folder**, shown under On My iPhone in the Files app (`UIFileSharingEnabled`, `LSSupportsOpeningDocumentsInPlace`). Settings (not onboarding) can point them at another folder, for storage reasons: folder picker plus a stored security-scoped bookmark; when the bookmark goes stale, fall back to the Received folder and say so.
- Saving without asking applies only to a sender that proved itself with its certificate; any other sender is asked about first.
- No history list, no retention period, and nothing received is synced through CloudKit.

## Not in scope

Claude chats (CodeRight), PIN, LocalSend compatibility on the phone, a separate Pulse phone app.
