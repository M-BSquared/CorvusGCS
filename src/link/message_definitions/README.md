# MAVLink message definitions

The XML files in `v1.0/` are the MAVLink project's message definitions
(<https://github.com/mavlink/mavlink>, MIT licence), the set pymavlink 2.4.50
generates its `all` dialect from. `build.rs` reads `v1.0/all.xml` and the
files it includes at build time. Update them together, as one set, and run
the parity tests afterwards.
