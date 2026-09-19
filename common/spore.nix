# The Rust /init: one static musl binary that does the whole bootstrap.
{ pkgsStatic, lib }:
pkgsStatic.rustPlatform.buildRustPackage {
  pname = "spore";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ./spore;
    fileset = lib.fileset.unions [
      ./spore/Cargo.toml
      ./spore/Cargo.lock
      ./spore/src
    ];
  };
  cargoLock.lockFile = ./spore/Cargo.lock;
  # db.rs only inserts rows. The -U flags undo the extensions that the
  # libsqlite3-sys bundled build turns on; this saves 400 KB.
  env.LIBSQLITE3_FLAGS = lib.concatStringsSep " " [
    "-USQLITE_ENABLE_FTS3"
    "-USQLITE_ENABLE_FTS3_PARENTHESIS"
    "-USQLITE_ENABLE_FTS5"
    "-USQLITE_ENABLE_RTREE"
    "-USQLITE_ENABLE_DBSTAT_VTAB"
    "-USQLITE_ENABLE_STAT4"
    "-USQLITE_ENABLE_COLUMN_METADATA"
    "-USQLITE_ENABLE_LOAD_EXTENSION"
    "-USQLITE_ENABLE_JSON1"
    "-USQLITE_SOUNDEX"
    "-DSQLITE_OMIT_LOAD_EXTENSION"
    "-DSQLITE_OMIT_DEPRECATED"
    "-DSQLITE_OMIT_SHARED_CACHE"
    "-DSQLITE_OMIT_JSON"
    "-DSQLITE_OMIT_PROGRESS_CALLBACK"
    "-DSQLITE_DEFAULT_MEMSTATUS=0"
    "-DSQLITE_DQS=0"
  ];
  # cargo strip = true keeps the symbol table under nixpkgs' cross build
  postInstall = "$STRIP -s $out/bin/spore";
  meta.mainProgram = "spore";
}
