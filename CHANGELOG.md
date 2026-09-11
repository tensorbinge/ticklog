# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2] - 2026-09-11

### Added

- Thread metadata in every record: the id and name of the thread that logged it.
  Both are selectable in a log-line pattern with `{thread_id}` and
  `{thread_name}`.
- A `format` key in `configure!` for custom log-line patterns. The available
  placeholders are `{timestamp}`, `{level}`, `{file}`, `{line}`, `{thread_id}`,
  `{thread_name}`, and `{message}`. Every placeholder accepts an optional format
  spec except `{timestamp}` and `{message}`, which take none. The default
  pattern is `{timestamp} {level} {file}:{line} {message}`.

### Changed

- The default log line separates the timestamp and the level with one space
  rather than two:

  ```text
  2026-09-11T15:06:42.808227159Z INFO src/main.rs:12 listening
  ```

- Thread names are no longer optional and default to `<unnamed>`.
- A log-line pattern no longer needs a `'static` lifetime.

### Fixed

- A log-line pattern that repeats the `{message}` placeholder is rejected.
- Truncating an over-long thread name no longer splits a multi-byte UTF-8
  character.
- Optional record sections are written only when their header flag is set, so a
  record with no source location is no longer misparsed.

## [0.1.1] - 2026-07-26

### Added

- `configure!`, a macro that reads the logger configuration at compile time:

  ```rust
  let _guard = ticklog::configure! {
      sink: ConsoleSink::stderr(),
      max_level: Level::Trace,
  }
  .unwrap();
  ```

### Removed

- `builder()`, `Builder`, and `init()`, replaced by `configure!`. Configuration
  is no longer assembled at runtime.

### Changed

- Relicensed from MIT to `MIT OR Apache-2.0`.

### Performance

- Records are written directly into the ring buffer, removing an intermediate
  copy on the calling thread.
- The timestamp is acquired after a ring buffer slot is reserved, rather than
  before.

### Fixed

- A record whose declared total size does not match its header is rejected when
  draining, instead of being parsed past its end.
- Ring buffer pointers are reconstructed with strict provenance.

## [0.1.0] - 2026-07-15

### Added

- Initial release. `trace!`, `debug!`, `info!`, `warn!`, and `error!` encode a
  record and hand it to a per-thread ring buffer, with no allocation,
  formatting, or I/O on the calling thread. A background drain thread decodes
  records and writes formatted lines to a sink.
- `ConsoleSink`, `FileSink`, and `WriterSink` write to stdout, stderr, files,
  and any `io::Write`. `FanOut` dispatches one record to several sinks, and
  `with_max_level` filters a sink by level. Custom destinations implement the
  `LogSink` trait.
- `Level` sets the level ceiling and `Backpressure` sets what a logging thread
  does when its buffer is full.
- `pin_thread` pins the calling thread to a set of logical CPUs and `warm_up`
  pre-allocates a thread's buffer, keeping the first log call off a
  latency-sensitive path.

[Unreleased]: https://github.com/tensorbinge/ticklog/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/tensorbinge/ticklog/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/tensorbinge/ticklog/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tensorbinge/ticklog/releases/tag/v0.1.0
