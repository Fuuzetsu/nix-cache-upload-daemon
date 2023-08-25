A simple nix binary cache upload daemon.

Often, when building nix packages, one would then use a post-build hook to sign
and upload the built paths artifacts. An example on how to do this is shown in
[official
documentation](https://nixos.org/manual/nix/unstable/advanced-topics/post-build-hook.html).

There are
[caveats](https://nixos.org/manual/nix/unstable/advanced-topics/post-build-hook.html#implementation-caveats)
however. Notably, doing this blocks the build loop so your machine can sit there
signing paths/uploading results while not doing useful building work in
meantime.

The documentation suggests:

>A more advanced implementation might pass the store paths to a user-supplied
>daemon or queue for processing the store paths outside of the build loop.

This is that "daemon".

The design is very simple: start the process, making note od the process ID. In
the post-build hook, simply feed the paths to the `stdin` of the process. That's
it. The process then takes care of signing and uploading all the paths, without
blocking the builds.

Once done, send SIGTERM to the process. It'll exit once it's done with whatever
left-over work.