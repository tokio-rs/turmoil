///! The only purpose of this file is to run the README.md as if it was a
///! doctest. The reason for a standalone file is so that any failures show up
///! clearly as 'readme' failures, as opposed to tests in lib.rs or some other
///! file.
///!
///! https://blog.guillaume-gomez.fr/articles/2020-03-07+cfg%28doctest%29+is+stable+and+you+should+use+it

doc_comment::doctest!("../README.md");
