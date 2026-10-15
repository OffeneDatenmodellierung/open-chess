# Privacy

*For the open-chess server and the open-chess app for iPhone and iPad.*

open-chess is a chess server and the apps that use it. Whoever runs a
server (its operator) holds what is described here; the app sends nothing
anywhere else, except to Lichess when you sign in to Lichess in it, and to
a streaming service you set up yourself.

## What is kept, and why

| What | Why | How long |
|---|---|---|
| Your account: username, a password hash or passkeys, your email if you give one | Signing in, telling you about your account | Until you close it |
| If you sign in with Apple: Apple's id for you, the email Apple shares (perhaps a private relay address), and Apple's token for the link | Signing you in with Apple; ending the link with Apple | Until you unlink it or close your account, when Apple is told |
| Your games: moves, times, results, ratings | Playing, ratings, your history; fair-play checks | Kept with your games after you close your account, under a placeholder name, as they're the other players' history too |
| Games played offline and recorded on a real board | The same, once uploaded | As above |
| Board photos and video frames you choose to send, with the position they show | Training and testing the board reader, under the [photo licence](photo-licence.md) | As the licence says; your name is removed when you close your account |
| The app's push token (if you turn notifications on) | Telling you it's your move, or that a result needs confirming | Until you turn them off, sign out or close your account |
| Server logs (address, time, request) | Running the service and keeping it safe | A short time, as the operator sets |

Nothing is used for advertising, sold, or used to track you across other
apps and websites.

## In the app

- **Offline**: games, settings and anything waiting to be sent stay on
  your device until you sync.
- **The camera** reads a board on the device. A photo leaves it only if
  you send it (with the photo licence accepted), or as part of a stream
  you start.
- **Bluetooth** is used only to talk to your chess board.
- **The microphone** is used only while you stream, and goes only to the
  streaming service you set up.
- **Lichess**: if you sign in to Lichess, the app keeps Lichess's token on
  your device and talks to Lichess directly; Lichess's own privacy policy
  applies there.
- **Streaming**: the picture and sound go from your phone straight to the
  service you choose (Twitch, YouTube or your own), under its terms. The
  stream key stays on your device.

## Your choices

- Close your account in the app (Account › Delete account) or on the
  server's Account page: games in progress are resigned, your sign-in
  details, tokens, passkeys and devices are removed.
- Change or withdraw your answer to the photo licence at any time; photos
  already given stay under the terms they were given.
- Turn notifications, low-data mode and photo uploads on or off in the
  app's settings.

## Contact

Ask the operator of the server you use. For the open-chess project:
[github.com/OffeneDatenmodellierung/open-chess](https://github.com/OffeneDatenmodellierung/open-chess/issues).
