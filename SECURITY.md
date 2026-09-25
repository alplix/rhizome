# Security

## Reporting a vulnerability

Please report security problems privately, through GitHub's *Report a
vulnerability* button on the repository's Security tab, rather than in a public
issue. Include the version, what you did and what happened. There is no bounty;
you will be credited in the changelog if you wish.

## What is in scope

Rhizome shows text from strangers, so the interesting classes are:

- message content becoming markup or script in the window;
- a link opening something other than an `http(s)` page;
- the window navigating away from the application;
- an outgoing line containing more than one IRC command (injection);
- a password appearing in a log, a file in the data directory, or `Debug` output;
- a crafted log or settings file damaging data or running code.

The defences for each are listed under *Safety properties* in the README and are
exercised by tests, including an end-to-end run in the real window.

## Not in scope

The installer is not code-signed (Windows SmartScreen will warn), and there is no
auto-updater: both are known and listed in the README.
