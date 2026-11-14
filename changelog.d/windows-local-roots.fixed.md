- Local storage roots work on Windows: `cluster plan` and `cluster apply`
  against a local directory, `omnigraph-server --cluster` on that directory,
  and `branch create` or `load --from` on a local graph no longer fail with
  `invalid file uri 'file://\\?\C:\...'` or `Invalid table location`. A local
  root whose path cannot be written as `file://` and read back unchanged (a
  path containing `?`, `#` or a percent-escape, on any platform) is now
  refused as an unsupported local storage root instead of resolving to a
  different directory.
