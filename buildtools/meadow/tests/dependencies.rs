//! Git dependencies, end to end: fetched, built against, and pinned.
//!
//! The repository is made here rather than fetched from anywhere, so these need
//! no network -- only a `git` on the path, without which they skip.

use meadow::git::Net;
use meadow::lock::Lock;
use meadow::package::{DepSource, Dependency, GitRef, PackageGraph, Resolver};
use meadow::pipeline;
use meadow_eval as eval;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, emptied first.
fn scratch(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-dep-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn git(args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A one-commit package with the tag `v1.0.0`, as a repository to depend on.
/// `None` when there is no usable git, which is the signal to skip.
fn dependency_repo(what: &str, answer: &str) -> Option<PathBuf> {
    let dir = scratch(what);
    std::fs::create_dir_all(dir.join("src")).ok()?;
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\nname = \"greet\"\nversion = \"0.1.0\"\n",
    )
    .ok()?;
    std::fs::write(
        dir.join("src/Lib.mw"),
        format!("@pub fun greeting = \"{answer}\"\n"),
    )
    .ok()?;
    let d = dir.display().to_string();
    let ok = git(&["init", "--quiet", "-b", "main", &d])
        && git(&["-C", &d, "add", "-A"])
        && git(&[
            "-C",
            &d,
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--quiet",
            "-m",
            "first",
        ])
        && git(&["-C", &d, "tag", "v1.0.0"]);
    ok.then_some(dir)
}

/// A package depending on `repo` at `v1.0.0`.
fn app_using(what: &str, repo: &Path) -> PathBuf {
    let dir = scratch(what);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ngreet = {{ git = \"{}\", tag = \"v1.0.0\" }}\n",
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src/Main.mw"), "def main = greeting\n").unwrap();
    dir
}

/// Build `app` with a cache of this test's own.
///
/// A resolver rather than `MEADOW_HOME`: that variable also says where the
/// runtime library and the extracted standard library are, so setting it would
/// move those for whatever else is running at the same time.
fn build_in(app: &Path, cache: &str, set: impl FnOnce(&mut Resolver)) -> pipeline::BuildOutput {
    let mut resolver = Resolver::for_entry(app);
    resolver.cache = scratch(&format!("cache-{cache}"));
    set(&mut resolver);
    pipeline::build_resolved(app, meadow::Options::debug(), &mut resolver)
}

#[test]
fn a_git_dependency_is_fetched_built_against_and_pinned() {
    let Some(repo) = dependency_repo("repo1", "hello from git") else {
        eprintln!("skipped: no usable git");
        return;
    };
    let app = app_using("app1", &repo);

    let out = build_in(&app, "one", |_| {});
    assert!(
        out.diagnostics.is_empty(),
        "{:?}",
        out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let linked = out.linked.expect("linked program");
    assert_eq!(
        eval::run(&linked.program).unwrap().to_string(),
        "\"hello from git\""
    );

    // The build wrote down what it used.
    let lock = Lock::load(&app);
    assert_eq!(lock.packages.len(), 1, "{:?}", lock.packages);
    let pinned = &lock.packages[0];
    assert_eq!(pinned.name, "greet");
    assert!(pinned.source.starts_with("git+"), "{}", pinned.source);
    assert!(pinned.source.ends_with("?tag=v1.0.0"), "{}", pinned.source);
    assert_eq!(pinned.rev.len(), 40, "a full commit name");
    assert!(!pinned.tree.is_empty(), "a tree hash");
}

#[test]
fn a_second_build_needs_no_network() {
    let Some(repo) = dependency_repo("repo2", "cached") else {
        eprintln!("skipped: no usable git");
        return;
    };
    let app = app_using("app2", &repo);

    let cache = scratch("cache-two");
    let first = {
        let mut r = Resolver::for_entry(&app);
        r.cache = cache.clone();
        pipeline::build_resolved(&app, meadow::Options::debug(), &mut r)
    };
    assert!(first.diagnostics.is_empty(), "{:?}", first.diagnostics);

    // The same graph again, offline and against the same cache. What was
    // pinned is already unpacked, so nothing has to be fetched.
    let mut resolver = Resolver::for_entry(&app);
    resolver.cache = cache;
    resolver.net = Net::Offline;
    let graph = PackageGraph::build_all_with(&[&app], &mut resolver)
        .expect("an offline build of what is already cached");
    assert!(
        graph.packages.iter().any(|p| &*p.name == "greet"),
        "the dependency is in the graph"
    );
}

#[test]
fn a_locked_build_refuses_a_dependency_that_is_not_pinned() {
    let Some(repo) = dependency_repo("repo3", "unpinned") else {
        eprintln!("skipped: no usable git");
        return;
    };
    let app = app_using("app3", &repo);

    // No lockfile has been written, so there is nothing to build against.
    let mut resolver = Resolver::for_entry(&app);
    resolver.cache = scratch("cache-three");
    resolver.locked = true;
    let err = PackageGraph::build_all_with(&[&app], &mut resolver)
        .expect_err("`--locked` with nothing pinned");
    assert!(err.msg.contains("meadow.lock"), "{}", err.msg);
}

#[test]
fn a_tag_that_moved_stops_a_build_that_pinned_the_old_one() {
    let Some(repo) = dependency_repo("repo4", "before") else {
        eprintln!("skipped: no usable git");
        return;
    };
    let app = app_using("app4", &repo);

    // One machine builds and pins.
    let out = build_in(&app, "four-before", |_| {});
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let pinned = Lock::load(&app).packages[0].rev.clone();

    // The tag is moved under it, as a force-push would move it.
    let d = repo.display().to_string();
    std::fs::write(repo.join("src/Lib.mw"), "@pub fun greeting = \"after\"\n").unwrap();
    assert!(git(&["-C", &d, "add", "-A"]));
    assert!(git(&[
        "-C",
        &d,
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "--quiet",
        "-m",
        "second",
    ]));
    assert!(git(&["-C", &d, "tag", "-f", "v1.0.0"]));

    // Another machine builds from that lockfile with nothing cached -- what CI
    // does. The tag no longer names what was pinned, so it stops.
    let out = build_in(&app, "four-after", |_| {});
    let said = out
        .diagnostics
        .iter()
        .map(|d| d.msg.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(said.contains("now names commit"), "{said}");
    // And the commit it was pinned to is named, so the reader can see which.
    assert!(said.contains(&pinned[..9]), "{said}");
}

#[test]
fn a_path_dependency_is_not_pinned() {
    // There is nothing to pin: a directory is whatever is in it. Only git
    // dependencies belong in the lockfile.
    let dir = scratch("paths");
    let dep = dir.join("util");
    std::fs::create_dir_all(dep.join("src")).unwrap();
    std::fs::write(
        dep.join("meadow.toml"),
        "[package]\nname = \"util\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(dep.join("src/Lib.mw"), "@pub fun answer = 42\n").unwrap();

    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("meadow.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nutil = { path = \"../util\" }\n",
    )
    .unwrap();
    std::fs::write(app.join("src/Main.mw"), "def main = answer\n").unwrap();

    let out = build_in(&app, "paths", |_| {});
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert!(
        !app.join("meadow.lock").exists(),
        "a path dependency should not write a lockfile"
    );
}

#[test]
fn a_manifest_can_name_every_way_of_choosing_a_commit() {
    let dir = scratch("manifest");
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
         a = { git = \"https://e.com/a\" }\n\
         b = { git = \"https://e.com/b\", branch = \"dev\" }\n\
         c = { git = \"https://e.com/c\", tag = \"v1\" }\n\
         d = { git = \"https://e.com/d\", rev = \"abc123\" }\n",
    )
    .unwrap();
    let m = meadow::package::Manifest::load(&dir).unwrap().unwrap();
    let want = |name: &str, r: GitRef| Dependency {
        name: name.to_string(),
        source: DepSource::Git {
            url: format!("https://e.com/{name}"),
            reference: r,
        },
    };
    assert_eq!(m.deps.len(), 4, "{:?}", m.deps);
    assert_eq!(m.deps[0], want("a", GitRef::Default));
    assert_eq!(m.deps[1], want("b", GitRef::Branch("dev".into())));
    assert_eq!(m.deps[2], want("c", GitRef::Tag("v1".into())));
    assert_eq!(m.deps[3], want("d", GitRef::Rev("abc123".into())));
}

#[test]
fn naming_two_ways_of_choosing_a_commit_is_refused() {
    // Which was meant cannot be guessed, so it is said rather than picked.
    let dir = scratch("conflict");
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
         a = { git = \"https://e.com/a\", branch = \"main\", tag = \"v1\" }\n",
    )
    .unwrap();
    let m = meadow::package::Manifest::load(&dir).unwrap().unwrap();
    assert!(m.deps.is_empty(), "{:?}", m.deps);
    assert!(
        m.warnings.iter().any(|w| w.contains("only one of")),
        "{:?}",
        m.warnings
    );
}

#[test]
fn update_moves_a_branch_forward_and_a_build_does_not() {
    let Some(repo) = dependency_repo("repo5", "first") else {
        eprintln!("skipped: no usable git");
        return;
    };
    // Following the branch, not the tag: a branch is what moves.
    let app = scratch("app5");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("meadow.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ngreet = {{ git = \"{}\", branch = \"main\" }}\n",
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(app.join("src/Main.mw"), "def main = greeting\n").unwrap();

    let cache = scratch("cache-five");
    let build = |set: fn(&mut Resolver)| {
        let mut r = Resolver::for_entry(&app);
        r.cache = cache.clone();
        set(&mut r);
        pipeline::build_resolved(&app, meadow::Options::debug(), &mut r)
    };

    let out = build(|_| {});
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(
        eval::run(&out.linked.unwrap().program).unwrap().to_string(),
        "\"first\""
    );
    let pinned = Lock::load(&app).packages[0].rev.clone();

    // The branch moves.
    let d = repo.display().to_string();
    std::fs::write(repo.join("src/Lib.mw"), "@pub fun greeting = \"second\"\n").unwrap();
    assert!(git(&["-C", &d, "add", "-A"]));
    assert!(git(&[
        "-C",
        &d,
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "--quiet",
        "-m",
        "two",
    ]));

    // An ordinary build must not follow it: the lockfile says which commit.
    let out = build(|_| {});
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(
        eval::run(&out.linked.unwrap().program).unwrap().to_string(),
        "\"first\"",
        "a build followed the branch instead of the lockfile"
    );

    // Updating is how it moves, and it is then pinned to the new commit.
    let out = build(|r| r.update = true);
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(
        eval::run(&out.linked.unwrap().program).unwrap().to_string(),
        "\"second\""
    );
    let now = Lock::load(&app).packages[0].rev.clone();
    assert_ne!(now, pinned, "the lockfile still names the old commit");
}

// --- releases ---------------------------------------------------------------

/// A repository whose every release tags a `label` saying which it is.
fn released_repo(what: &str, releases: &[&str]) -> Option<PathBuf> {
    let dir = scratch(what);
    std::fs::create_dir_all(dir.join("src")).ok()?;
    let d = dir.display().to_string();
    if !git(&["init", "--quiet", "-b", "main", &d]) {
        return None;
    }
    for release in releases {
        std::fs::write(
            dir.join("meadow.toml"),
            format!("[package]\nname = \"widget\"\nversion = \"{release}\"\n"),
        )
        .ok()?;
        std::fs::write(
            dir.join("src/Lib.mw"),
            format!("@pub def label = \"{release}\"\n\n@pub data Tag = Tag Int\n"),
        )
        .ok()?;
        let ok = git(&["-C", &d, "add", "-A"])
            && git(&[
                "-C",
                &d,
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-m",
                release,
            ])
            && git(&["-C", &d, "tag", &format!("v{release}")]);
        if !ok {
            return None;
        }
    }
    Some(dir)
}

/// An app wanting `version` of `repo`, with a `main` that shows which release
/// it was built against.
fn app_wanting(what: &str, repo: &Path, version: &str) -> PathBuf {
    let dir = scratch(what);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nwidget = {{ git = \"{}\", version = \"{version}\" }}\n",
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("src/Main.mw"),
        "use widget (label)\n\ndef main = label\n",
    )
    .unwrap();
    dir
}

fn ran(out: pipeline::BuildOutput) -> String {
    let linked = out.linked.expect("a linked program");
    eval::run(&linked.program).expect("it runs").to_string()
}

#[test]
fn a_version_takes_the_newest_release_that_does_not_break_it() {
    let Some(repo) = released_repo("rel-newest", &["0.1.0", "0.1.1", "0.2.0"]) else {
        return;
    };
    let app = app_wanting("rel-newest-app", &repo, "0.1.0");
    let out = build_in(&app, "rel-newest", |_| {});
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    // 0.1.1, not 0.2.0: below 1.0 the minor is the breaking digit.
    assert_eq!(ran(out), "\"0.1.1\"");
}

#[test]
fn the_release_a_version_resolved_to_is_written_to_the_lockfile() {
    let Some(repo) = released_repo("rel-lock", &["1.0.0", "1.1.0"]) else {
        return;
    };
    let app = app_wanting("rel-lock-app", &repo, "1.0.0");
    let out = build_in(&app, "rel-lock", |_| {});
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    let lock = Lock::load(&app);
    let entry = lock
        .packages
        .iter()
        .find(|p| p.name == "widget")
        .expect("a locked widget");
    assert_eq!(entry.version.as_deref(), Some("1.1.0"));
    assert!(entry.source.ends_with("?version=1.0.0"), "{}", entry.source);
}

#[test]
fn a_version_with_no_release_to_meet_it_says_what_there_is() {
    let Some(repo) = released_repo("rel-none", &["0.1.0", "0.2.0"]) else {
        return;
    };
    let app = app_wanting("rel-none-app", &repo, "3.0.0");
    let out = build_in(&app, "rel-none", |_| {});
    let said = format!("{:?}", out.diagnostics);
    assert!(said.contains("no release of `widget`"), "{said}");
    assert!(said.contains("0.2.0"), "{said}");
}

#[test]
fn a_repository_with_no_releases_says_so_rather_than_guessing() {
    let Some(repo) = dependency_repo("rel-untagged", "hi") else {
        return;
    };
    // The repository has `v1.0.0`, so ask for something no tag can meet: a
    // release of a repository that tags none is the case this names.
    let dir = scratch("rel-untagged-app");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\ngreet = {{ git = \"{}\", version = \"2.0.0\" }}\n",
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src/Main.mw"), "def main = greeting\n").unwrap();
    let out = build_in(&dir, "rel-untagged", |_| {});
    let said = format!("{:?}", out.diagnostics);
    assert!(said.contains("no release of `greet`"), "{said}");
}

#[test]
fn everything_that_can_share_a_release_shares_one() {
    let Some(repo) = released_repo("rel-share", &["1.0.0", "1.2.0", "1.4.0"]) else {
        return;
    };
    // Two libraries want different releases of one package, and both are met
    // by the newest: the build takes one copy, not two.
    let dir = scratch("rel-share-app");
    for (lib, want) in [("one", "1.0.0"), ("two", "1.2.0")] {
        let at = dir.join(lib);
        std::fs::create_dir_all(at.join("src")).unwrap();
        std::fs::write(
            at.join("meadow.toml"),
            format!(
                "[package]\nname = \"{lib}\"\nversion = \"0.1.0\"\n\n\
                 [dependencies]\nwidget = {{ git = \"{}\", version = \"{want}\" }}\n",
                repo.display()
            ),
        )
        .unwrap();
        std::fs::write(
            at.join("src/Lib.mw"),
            format!("use widget (label)\n\n@pub def {lib}Label = label\n"),
        )
        .unwrap();
    }
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\none = { path = \"one\" }\ntwo = { path = \"two\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/Main.mw"),
        "use one (oneLabel)\nuse two (twoLabel)\n\ndef main = (oneLabel, twoLabel)\n",
    )
    .unwrap();
    let out = build_in(&dir, "rel-share", |_| {});
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    assert_eq!(ran(out), "(\"1.4.0\", \"1.4.0\")");
    let graph = PackageGraph::build(&dir).expect("a graph");
    let copies = graph
        .packages
        .iter()
        .filter(|p| p.name.to_string() == "widget")
        .count();
    assert_eq!(copies, 1, "one release serves both");
}

#[test]
fn requirements_that_cannot_meet_take_a_copy_each() {
    let Some(repo) = released_repo("rel-two", &["0.1.0", "0.2.0"]) else {
        return;
    };
    let dir = scratch("rel-two-app");
    for (lib, want) in [("old", "0.1.0"), ("new", "0.2.0")] {
        let at = dir.join(lib);
        std::fs::create_dir_all(at.join("src")).unwrap();
        std::fs::write(
            at.join("meadow.toml"),
            format!(
                "[package]\nname = \"{lib}\"\nversion = \"0.1.0\"\n\n\
                 [dependencies]\nwidget = {{ git = \"{}\", version = \"{want}\" }}\n",
                repo.display()
            ),
        )
        .unwrap();
        std::fs::write(
            at.join("src/Lib.mw"),
            format!("use widget (label)\n\n@pub def {lib}Label = label\n"),
        )
        .unwrap();
    }
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nold = { path = \"old\" }\nnew = { path = \"new\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/Main.mw"),
        "use old (oldLabel)\nuse new (newLabel)\n\ndef main = (oldLabel, newLabel)\n",
    )
    .unwrap();
    let out = build_in(&dir, "rel-two", |_| {});
    assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    // `0.1` and `0.2` cannot be met by one release, so both are built, and
    // each library gets the one it asked for.
    assert_eq!(ran(out), "(\"0.1.0\", \"0.2.0\")");
}

#[test]
fn two_copies_of_a_package_declare_two_types() {
    let Some(repo) = released_repo("rel-types", &["0.1.0", "0.2.0"]) else {
        return;
    };
    let dir = scratch("rel-types-app");
    std::fs::create_dir_all(dir.join("maker/src")).unwrap();
    std::fs::write(
        dir.join("maker/meadow.toml"),
        format!(
            "[package]\nname = \"maker\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nwidget = {{ git = \"{}\", version = \"0.1.0\" }}\n",
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("maker/src/Lib.mw"),
        "use widget (Tag)\nuse widget.Tag.*\n\n@pub def aTag = Tag 1\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("taker/src")).unwrap();
    std::fs::write(
        dir.join("taker/meadow.toml"),
        format!(
            "[package]\nname = \"taker\"\nversion = \"0.1.0\"\n\n\
             [dependencies]\nwidget = {{ git = \"{}\", version = \"0.2.0\" }}\n",
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("taker/src/Lib.mw"),
        "use widget (Tag)\n\n@pub fun takeTag (t : Tag) = 1\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nmaker = { path = \"maker\" }\ntaker = { path = \"taker\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/Main.mw"),
        "use maker (aTag)\nuse taker (takeTag)\n\ndef main = takeTag aTag\n",
    )
    .unwrap();
    let out = build_in(&dir, "rel-types", |_| {});
    let said = format!("{:?}", out.diagnostics);
    // One package's `Tag` is not the other's, and the message says which is
    // which by version.
    assert!(said.contains("type mismatch"), "{said}");
    assert!(said.contains("widget@0.1.0"), "{said}");
    assert!(said.contains("widget@0.2.0"), "{said}");
}

#[test]
fn a_version_dependency_reads_back_as_one() {
    let dir = scratch("rel-manifest");
    std::fs::write(
        dir.join("meadow.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
         a = { git = \"https://e.com/a\", version = \"1.2.0\" }\n",
    )
    .unwrap();
    let m = meadow::package::Manifest::load(&dir).unwrap().unwrap();
    assert_eq!(
        m.deps[0],
        Dependency {
            name: "a".to_string(),
            source: DepSource::Git {
                url: "https://e.com/a".to_string(),
                reference: GitRef::Version(meadow::semver::Req::parse("1.2.0").unwrap()),
            },
        }
    );
}
