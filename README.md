Overview
========

Borgify is an opinionated wrapper to run [Borg
backup](https://borgbackup.readthedocs.io/) on multiple source directories
listed with a configuration file, with optional btrfs snapshotting to make
backing up live filesystems safe.


Config file
===========

Borgify reads its configuration from `/etc/borgify.json`. The top-level
document must be of object type. It must have a key named `archives`, and may
additionally have a key named `defaults`.

`defaults` section
------------------

The `defaults` section, if present, must be of object type and contains default
values for keys in individual archive items. If a required key is not specified
in the `defaults` section, it must be specified for each archive. If a key is
specified in the `defaults` section, it may be omitted (even if required) in an
archive; if specified, the archive-specific value overrides the value from
`defaults`.

The following keys can be specified in the `defaults` section:
* `compression`
* `repository`

`archives` section
------------------

The `archives` section must be of object type. Each entry has a key which is a
name for the archive (used to name the Borg archives) and a value of object
type. The value contains the details of the archive. The following keys are
defined:
* `compression`: Required, string. The compression method and parameters to use
  to compress data. This value is passed to Borg’s
  [`--compression`](https://borgbackup.readthedocs.io/en/stable/usage/create.html)
  option.
* `repository`: Required, string. The URL of the repository where the backup
  data will be stored. This must have already been created via [`borg
  init`](https://borgbackup.readthedocs.io/en/stable/usage/init.html). If a
  filesystem path is used, it must be absolute (not relative).
* `root`: Required, string. The path to the top-level directory of the data to
  back up.
* `btrfs_snapshot`: Optional, boolean (absent is equivalent to `false`). If
  `true`, the path specified in `root` will be snapshotted before backup
  begins, Borg will be pointed at the snapshot to back up, and the snapshot
  will be deleted afterwards. The snapshot will be placed at a randomized name
  in the parent directory of the specified `root`.
* `patterns`: Array of string, optional (absent is equivalent to empty array).
  One or more [Borg include/exclude
  patterns](https://borgbackup.readthedocs.io/en/stable/usage/help.html#borg-patterns),
  each starting with either `+`, `-`, or `P` (`R` is prohibited), each of which
  will be passed to Borg via `--pattern`.


Operation
=========

Each time Borgify is invoked, it will do the following steps in order:
1. Read and parse the config file.
2. Verify that all repositories are available, and ask for a passphrase for
   each if necessary.
3. For each archive, run [`borg
   create`](https://borgbackup.readthedocs.io/en/stable/usage/create.html) to
   back up the specified files.


Borg invocation options
=======================

When `borg create` is invoked, Borgify passes the following options:
* `--verbose`
* `--progress`
* `--iec`
* `--umask 0027`
* `--stats`
* `--exclude-caches`
* `--timestamp` with the same timestamp for each archive in the run
* `--compression` with the value specified in the config file
* `--pattern` for each pattern specified in the config file

The archive name comprises the key in the `archives` object, a hyphen, and the
run timestamp (the same as passed to `--timestamp`), converted to your local
timezone.

The `BORG_FILES_CACHE_SUFFIX` environment variable will be set equal to the
archive name (aka the key in the `archives` section).
