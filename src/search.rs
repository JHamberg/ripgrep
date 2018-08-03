use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use grep::matcher::Matcher;
use grep::printer::{JSON, Standard, Summary, Stats};
use grep::regex::RegexMatcher;
use grep::searcher::Searcher;
use termcolor::WriteColor;

use decompressor::{DecompressionReader, is_compressed};
use preprocessor::PreprocessorReader;
use subject::Subject;

/// The configuration for the search worker. Among a few other things, the
/// configuration primarily controls the way we show search results to users
/// at a very high level.
#[derive(Clone, Debug)]
struct Config {
    preprocessor: Option<PathBuf>,
    search_zip: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            preprocessor: None,
            search_zip: false,
        }
    }
}

/// A builder for configuring and constructing a search worker.
#[derive(Clone, Debug)]
pub struct SearchWorkerBuilder {
    config: Config,
}

impl Default for SearchWorkerBuilder {
    fn default() -> SearchWorkerBuilder {
        SearchWorkerBuilder::new()
    }
}

impl SearchWorkerBuilder {
    /// Create a new builder for configuring and constructing a search worker.
    pub fn new() -> SearchWorkerBuilder {
        SearchWorkerBuilder { config: Config::default() }
    }

    /// Create a new search worker using the given searcher, matcher and
    /// printer.
    pub fn build<W: WriteColor>(
        &self,
        matcher: PatternMatcher,
        searcher: Searcher,
        printer: Printer<W>,
    ) -> SearchWorker<W> {
        let config = self.config.clone();
        SearchWorker { config, matcher, searcher, printer }
    }

    /// Set the path to a preprocessor command.
    ///
    /// When this is set, instead of searching files directly, the given
    /// command will be run with the file path as the first argument, and the
    /// output of that command will be searched instead.
    pub fn preprocessor(
        &mut self,
        cmd: Option<PathBuf>,
    ) -> &mut SearchWorkerBuilder {
        self.config.preprocessor = cmd;
        self
    }

    /// Enable the decompression and searching of common compressed files.
    ///
    /// When enabled, if a particular file path is recognized as a compressed
    /// file, then it is decompressed before searching.
    ///
    /// Note that if a preprocessor command is set, then it overrides this
    /// setting.
    pub fn search_zip(&mut self, yes: bool) -> &mut SearchWorkerBuilder {
        self.config.search_zip = yes;
        self
    }
}

/// The result of executing a search.
///
/// Generally speaking, the "result" of a search is sent to a printer, which
/// writes results to an underlying writer such as stdout or a file. However,
/// every search also has some aggregate statistics or meta data that may be
/// useful to higher level routines.
#[derive(Clone, Debug, Default)]
pub struct SearchResult {
    has_match: bool,
    stats: Option<Stats>,
}

impl SearchResult {
    /// Whether the search found a match or not.
    pub fn has_match(&self) -> bool {
        self.has_match
    }

    /// Return aggregate search statistics for a single search, if available.
    ///
    /// It can be expensive to compute statistics, so these are only present
    /// if explicitly enabled in the printer provided by the caller.
    pub fn stats(&self) -> Option<&Stats> {
        self.stats.as_ref()
    }
}

/// The pattern matcher used by a search worker.
#[derive(Clone, Debug)]
pub enum PatternMatcher {
    RustRegex(RegexMatcher),
}

/// The printer used by a search worker.
///
/// The `W` type parameter refers to the type of the underlying writer.
#[derive(Debug)]
pub enum Printer<W> {
    /// Use the standard printer, which supports the classic grep-like format.
    Standard(Standard<W>),
    /// Use the summary printer, which supports aggregate displays of search
    /// results.
    Summary(Summary<W>),
    /// A JSON printer, which emits results in the JSON Lines format.
    JSON(JSON<W>),
}

impl<W: WriteColor> Printer<W> {
    /// Print the given statistics to the underlying writer in a way that is
    /// consistent with this printer's format.
    ///
    /// While `Stats` contains a duration itself, this only corresponds to the
    /// time spent searching, where as `total_duration` should roughly
    /// approximate the lifespan of the ripgrep process itself.
    pub fn print_stats(
        &mut self,
        total_duration: Duration,
        stats: &Stats,
    ) -> io::Result<()> {
        match *self {
            Printer::JSON(_) => unimplemented!(),
            Printer::Standard(_) | Printer::Summary(_) => {
                self.print_stats_human(total_duration, stats)
            }
        }
    }

    fn print_stats_human(
        &mut self,
        total_duration: Duration,
        stats: &Stats,
    ) -> io::Result<()> {
        write!(
            self.get_mut(),
            "
{matches} matches
{lines} matched lines
{searches_with_match} files contained matches
{searches} files searched
{bytes_printed} bytes printed
{bytes_searched} bytes searched
{search_time:.6} seconds spent searching
{process_time:.6} seconds
",
            matches = stats.matches(),
            lines = stats.matched_lines(),
            searches_with_match = stats.searches_with_match(),
            searches = stats.searches(),
            bytes_printed = stats.bytes_printed(),
            bytes_searched = stats.bytes_searched(),
            search_time = fractional_seconds(stats.elapsed()),
            process_time = fractional_seconds(total_duration)
        )
    }

    /// Return a mutable reference to the underlying printer's writer.
    pub fn get_mut(&mut self) -> &mut W {
        match *self {
            Printer::Standard(ref mut p) => p.get_mut(),
            Printer::Summary(ref mut p) => p.get_mut(),
            Printer::JSON(ref mut p) => p.get_mut(),
        }
    }
}

/// A worker for executing searches.
///
/// It is intended for a single worker to execute many searches, and is
/// generally intended to be used from a single thread. When searching using
/// multiple threads, it is better to create a new worker for each thread.
#[derive(Debug)]
pub struct SearchWorker<W> {
    config: Config,
    matcher: PatternMatcher,
    searcher: Searcher,
    printer: Printer<W>,
}

impl<W: WriteColor> SearchWorker<W> {
    /// Execute a search over the given subject.
    pub fn search(&mut self, subject: &Subject) -> io::Result<SearchResult> {
        self.search_impl(subject)
    }

    /// Return a mutable reference to the underlying printer.
    pub fn printer(&mut self) -> &mut Printer<W> {
        &mut self.printer
    }

    /// Search the given subject using the appropriate strategy.
    fn search_impl(&mut self, subject: &Subject) -> io::Result<SearchResult> {
        let path = subject.path();
        if subject.is_stdin() {
            let stdin = io::stdin();
            // A `return` here appeases the borrow checker. NLL will fix this.
            return self.search_reader(path, stdin.lock());
        } else if self.config.preprocessor.is_some() {
            let cmd = self.config.preprocessor.clone().unwrap();
            let rdr = PreprocessorReader::from_cmd_path(cmd, path)?;
            self.search_reader(path, rdr)
        } else if self.config.search_zip && is_compressed(path) {
            match DecompressionReader::from_path(path) {
                None => Ok(SearchResult::default()),
                Some(rdr) => self.search_reader(path, rdr),
            }
        } else {
            self.search_path(path)
        }
    }

    /// Search the contents of the given file path.
    fn search_path(&mut self, path: &Path) -> io::Result<SearchResult> {
        use self::PatternMatcher::*;

        let (searcher, printer) = (&mut self.searcher, &mut self.printer);
        match self.matcher {
            RustRegex(ref m) => search_path(m, searcher, printer, path),
        }
    }

    /// Executes a search on the given reader, which may or may not correspond
    /// directly to the contents of the given file path. Instead, the reader
    /// may actually cause something else to be searched (for example, when
    /// a preprocessor is set or when decompression is enabled). In those
    /// cases, the file path is used for visual purposes only.
    ///
    /// Generally speaking, this method should only be used when there is no
    /// other choice. Searching via `search_path` provides more opportunities
    /// for optimizations (such as memory maps).
    fn search_reader<R: io::Read>(
        &mut self,
        path: &Path,
        rdr: R,
    ) -> io::Result<SearchResult> {
        use self::PatternMatcher::*;

        let (searcher, printer) = (&mut self.searcher, &mut self.printer);
        match self.matcher {
            RustRegex(ref m) => search_reader(m, searcher, printer, path, rdr),
        }
    }
}

/// Search the contents of the given file path using the given matcher,
/// searcher and printer.
fn search_path<M: Matcher, W: WriteColor>(
    matcher: M,
    searcher: &mut Searcher,
    printer: &mut Printer<W>,
    path: &Path,
) -> io::Result<SearchResult> {
    match *printer {
        Printer::Standard(ref mut p) => {
            let mut sink = p.sink_with_path(&matcher, path);
            searcher.search_path(&matcher, path, &mut sink)?;
            Ok(SearchResult {
                has_match: sink.has_match(),
                stats: sink.stats().map(|s| s.clone()),
            })
        }
        Printer::Summary(ref mut p) => {
            let mut sink = p.sink_with_path(&matcher, path);
            searcher.search_path(&matcher, path, &mut sink)?;
            Ok(SearchResult {
                has_match: sink.has_match(),
                stats: sink.stats().map(|s| s.clone()),
            })
        }
        Printer::JSON(ref mut p) => {
            let mut sink = p.sink_with_path(&matcher, path);
            searcher.search_path(&matcher, path, &mut sink)?;
            Ok(SearchResult {
                has_match: sink.has_match(),
                stats: Some(sink.stats().clone()),
            })
        }
    }
}

/// Search the contents of the given reader using the given matcher, searcher
/// and printer.
fn search_reader<M: Matcher, R: io::Read, W: WriteColor>(
    matcher: M,
    searcher: &mut Searcher,
    printer: &mut Printer<W>,
    path: &Path,
    rdr: R,
) -> io::Result<SearchResult> {
    match *printer {
        Printer::Standard(ref mut p) => {
            let mut sink = p.sink_with_path(&matcher, path);
            searcher.search_reader(&matcher, rdr, &mut sink)?;
            Ok(SearchResult {
                has_match: sink.has_match(),
                stats: sink.stats().map(|s| s.clone()),
            })
        }
        Printer::Summary(ref mut p) => {
            let mut sink = p.sink_with_path(&matcher, path);
            searcher.search_reader(&matcher, rdr, &mut sink)?;
            Ok(SearchResult {
                has_match: sink.has_match(),
                stats: sink.stats().map(|s| s.clone()),
            })
        }
        Printer::JSON(ref mut p) => {
            let mut sink = p.sink_with_path(&matcher, path);
            searcher.search_reader(&matcher, rdr, &mut sink)?;
            Ok(SearchResult {
                has_match: sink.has_match(),
                stats: Some(sink.stats().clone()),
            })
        }
    }
}

/// Return the given duration as fractional seconds.
fn fractional_seconds(duration: Duration) -> f64 {
    (duration.as_secs() as f64) + (duration.subsec_nanos() as f64 * 1e-9)
}
