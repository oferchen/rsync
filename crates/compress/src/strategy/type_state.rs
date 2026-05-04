//! Type-state pattern for compression negotiation lifecycle.
//!
//! Enforces at compile time that negotiation follows the correct sequence:
//!
//! ```text
//! NegotiationPipeline<Uninit>
//!     -> exchange_capabilities()
//! NegotiationPipeline<CapabilitiesExchanged>
//!     -> select_algorithm()
//! NegotiationPipeline<AlgorithmSelected>
//!     -> into_strategy()
//! Box<dyn CompressionStrategy>
//! ```
//!
//! Each state transition is a consuming method that returns the next state,
//! preventing out-of-order calls. Methods only available in the correct state
//! simply do not exist on other states.
//!
//! # Example
//!
//! ```
//! use compress::strategy::type_state::NegotiationPipeline;
//! use compress::strategy::negotiator::DefaultCompressionNegotiator;
//! use compress::zlib::CompressionLevel;
//!
//! let pipeline = NegotiationPipeline::new(
//!     Box::new(DefaultCompressionNegotiator::new()),
//!     CompressionLevel::Default,
//! );
//!
//! let exchanged = pipeline.exchange_capabilities(&["zlib", "none"]);
//! let selected = exchanged.select_algorithm(false);
//! let strategy = selected.into_strategy();
//! assert_eq!(strategy.algorithm_name(), "zlib");
//! ```

use std::fmt;
use std::marker::PhantomData;

use super::negotiator::CompressionNegotiator;
use super::{CompressionAlgorithmKind, CompressionStrategy, CompressionStrategySelector};
use crate::zlib::CompressionLevel;

/// Marker type for the initial state before capabilities are exchanged.
#[derive(Debug)]
pub struct Uninit;

/// Marker type for the state after local and remote capabilities have been exchanged.
#[derive(Debug)]
pub struct CapabilitiesExchanged;

/// Marker type for the state after an algorithm has been selected.
#[derive(Debug)]
pub struct AlgorithmSelected;

/// Type-state pipeline that enforces the compression negotiation lifecycle at
/// compile time.
///
/// The pipeline progresses through three states:
///
/// 1. [`Uninit`] - constructed with a negotiator and compression level.
/// 2. [`CapabilitiesExchanged`] - remote algorithm list has been provided.
/// 3. [`AlgorithmSelected`] - a mutual algorithm has been chosen.
///
/// State transitions are consuming methods, so the previous state becomes
/// inaccessible after each step.
///
/// # Compile-time safety
///
/// Calling methods out of order is a compile error. For example,
/// `select_algorithm` is only defined on `NegotiationPipeline<CapabilitiesExchanged>`,
/// so calling it on `NegotiationPipeline<Uninit>` will not compile.
pub struct NegotiationPipeline<S> {
    negotiator: Box<dyn CompressionNegotiator>,
    level: CompressionLevel,
    state_data: StateData,
    _state: PhantomData<S>,
}

impl<S> fmt::Debug for NegotiationPipeline<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NegotiationPipeline")
            .field("level", &self.level)
            .field("state_data", &self.state_data)
            .finish_non_exhaustive()
    }
}

/// Internal data that accumulates across state transitions.
#[derive(Debug, Clone)]
struct StateData {
    remote_algorithms: Vec<String>,
    selected_algorithm: Option<&'static str>,
}

impl StateData {
    const fn new() -> Self {
        Self {
            remote_algorithms: Vec::new(),
            selected_algorithm: None,
        }
    }
}

impl NegotiationPipeline<Uninit> {
    /// Creates a new negotiation pipeline in the [`Uninit`] state.
    ///
    /// The `negotiator` determines algorithm preference ordering and selection
    /// logic. The `level` is applied to whichever algorithm is ultimately
    /// selected.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::strategy::type_state::NegotiationPipeline;
    /// use compress::strategy::negotiator::DefaultCompressionNegotiator;
    /// use compress::zlib::CompressionLevel;
    ///
    /// let pipeline = NegotiationPipeline::new(
    ///     Box::new(DefaultCompressionNegotiator::new()),
    ///     CompressionLevel::Default,
    /// );
    /// ```
    pub fn new(negotiator: Box<dyn CompressionNegotiator>, level: CompressionLevel) -> Self {
        Self {
            negotiator,
            level,
            state_data: StateData::new(),
            _state: PhantomData,
        }
    }

    /// Returns the list of locally supported algorithms advertised by the
    /// negotiator.
    ///
    /// Callers typically send this list to the remote peer during the vstring
    /// exchange phase of the rsync protocol handshake.
    pub fn local_algorithms(&self) -> Vec<&'static str> {
        self.negotiator.supported_algorithms()
    }

    /// Transitions to [`CapabilitiesExchanged`] by recording the remote peer's
    /// advertised algorithm list.
    ///
    /// Consumes `self` and returns the pipeline in the next state.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::strategy::type_state::NegotiationPipeline;
    /// use compress::strategy::negotiator::DefaultCompressionNegotiator;
    /// use compress::zlib::CompressionLevel;
    ///
    /// let pipeline = NegotiationPipeline::new(
    ///     Box::new(DefaultCompressionNegotiator::new()),
    ///     CompressionLevel::Default,
    /// );
    /// let exchanged = pipeline.exchange_capabilities(&["zstd", "zlib", "none"]);
    /// ```
    pub fn exchange_capabilities(
        self,
        remote_algorithms: &[&str],
    ) -> NegotiationPipeline<CapabilitiesExchanged> {
        let mut state_data = self.state_data;
        state_data.remote_algorithms = remote_algorithms.iter().map(|s| (*s).to_owned()).collect();

        NegotiationPipeline {
            negotiator: self.negotiator,
            level: self.level,
            state_data,
            _state: PhantomData,
        }
    }
}

impl NegotiationPipeline<CapabilitiesExchanged> {
    /// Returns the remote peer's advertised algorithm list.
    pub fn remote_algorithms(&self) -> Vec<&str> {
        self.state_data
            .remote_algorithms
            .iter()
            .map(|s| s.as_str())
            .collect()
    }

    /// Transitions to [`AlgorithmSelected`] by running the negotiator's
    /// selection logic against the remote algorithm list.
    ///
    /// The `is_server` flag controls asymmetric selection semantics matching
    /// upstream rsync's `parse_negotiate_str()` - servers iterate the remote
    /// (client) list while clients iterate their own local list.
    ///
    /// Consumes `self` and returns the pipeline in the final state.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::strategy::type_state::NegotiationPipeline;
    /// use compress::strategy::negotiator::DefaultCompressionNegotiator;
    /// use compress::zlib::CompressionLevel;
    ///
    /// let pipeline = NegotiationPipeline::new(
    ///     Box::new(DefaultCompressionNegotiator::new()),
    ///     CompressionLevel::Default,
    /// );
    /// let exchanged = pipeline.exchange_capabilities(&["zlib", "none"]);
    /// let selected = exchanged.select_algorithm(false);
    /// assert_eq!(selected.selected_algorithm_name(), "zlib");
    /// ```
    pub fn select_algorithm(self, is_server: bool) -> NegotiationPipeline<AlgorithmSelected> {
        let remote_refs: Vec<&str> = self
            .state_data
            .remote_algorithms
            .iter()
            .map(|s| s.as_str())
            .collect();

        let selected = self.negotiator.select_algorithm(&remote_refs, is_server);

        let mut state_data = self.state_data;
        state_data.selected_algorithm = Some(selected);

        NegotiationPipeline {
            negotiator: self.negotiator,
            level: self.level,
            state_data,
            _state: PhantomData,
        }
    }
}

impl NegotiationPipeline<AlgorithmSelected> {
    /// Returns the name of the algorithm that was selected during negotiation.
    pub fn selected_algorithm_name(&self) -> &'static str {
        self.state_data
            .selected_algorithm
            .expect("algorithm must be set in AlgorithmSelected state")
    }

    /// Returns the [`CompressionAlgorithmKind`] for the selected algorithm.
    ///
    /// Returns `None` if the selected algorithm name does not map to a known
    /// kind (e.g., the negotiator returned `"none"`).
    pub fn selected_algorithm_kind(&self) -> Option<CompressionAlgorithmKind> {
        CompressionAlgorithmKind::from_name(self.selected_algorithm_name())
    }

    /// Consumes the pipeline and produces the final [`CompressionStrategy`].
    ///
    /// Maps the selected algorithm name to a concrete strategy implementation.
    /// Falls back to no-compression if the algorithm is unknown or unavailable.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::strategy::type_state::NegotiationPipeline;
    /// use compress::strategy::negotiator::FixedCompressionNegotiator;
    /// use compress::zlib::CompressionLevel;
    ///
    /// let pipeline = NegotiationPipeline::new(
    ///     Box::new(FixedCompressionNegotiator::new("zlib")),
    ///     CompressionLevel::Best,
    /// );
    /// let strategy = pipeline
    ///     .exchange_capabilities(&["zlib", "none"])
    ///     .select_algorithm(false)
    ///     .into_strategy();
    /// assert_eq!(strategy.algorithm_name(), "zlib");
    /// ```
    pub fn into_strategy(self) -> Box<dyn CompressionStrategy> {
        let name = self.selected_algorithm_name();
        match CompressionAlgorithmKind::from_name(name) {
            Some(kind) if kind.is_available() => {
                CompressionStrategySelector::for_algorithm(kind, self.level)
                    .unwrap_or_else(|_| Box::new(super::NoCompressionStrategy::new()))
            }
            _ => Box::new(super::NoCompressionStrategy::new()),
        }
    }

    /// Returns the compression level that will be applied to the strategy.
    pub fn compression_level(&self) -> CompressionLevel {
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::negotiator::{DefaultCompressionNegotiator, FixedCompressionNegotiator};

    fn default_pipeline() -> NegotiationPipeline<Uninit> {
        NegotiationPipeline::new(
            Box::new(DefaultCompressionNegotiator::new()),
            CompressionLevel::Default,
        )
    }

    fn fixed_pipeline(algo: &'static str) -> NegotiationPipeline<Uninit> {
        NegotiationPipeline::new(
            Box::new(FixedCompressionNegotiator::new(algo)),
            CompressionLevel::Default,
        )
    }

    #[test]
    fn full_lifecycle_selects_zlib() {
        let strategy = default_pipeline()
            .exchange_capabilities(&["zlib", "none"])
            .select_algorithm(false)
            .into_strategy();
        assert_eq!(strategy.algorithm_name(), "zlib");
    }

    #[test]
    fn full_lifecycle_selects_none_when_no_match() {
        let strategy = default_pipeline()
            .exchange_capabilities(&["brotli", "snappy"])
            .select_algorithm(false)
            .into_strategy();
        assert_eq!(strategy.algorithm_name(), "none");
    }

    #[test]
    fn full_lifecycle_with_fixed_negotiator() {
        let strategy = fixed_pipeline("zlib")
            .exchange_capabilities(&["zstd", "zlib", "none"])
            .select_algorithm(false)
            .into_strategy();
        assert_eq!(strategy.algorithm_name(), "zlib");
    }

    #[test]
    fn full_lifecycle_fixed_no_match_returns_none() {
        let strategy = fixed_pipeline("zlib")
            .exchange_capabilities(&["zstd"])
            .select_algorithm(false)
            .into_strategy();
        assert_eq!(strategy.algorithm_name(), "none");
    }

    #[test]
    fn full_lifecycle_algorithm_none_selected() {
        let strategy = fixed_pipeline("none")
            .exchange_capabilities(&["none"])
            .select_algorithm(false)
            .into_strategy();
        assert_eq!(strategy.algorithm_name(), "none");
    }

    #[test]
    fn uninit_local_algorithms_returns_negotiator_list() {
        let pipeline = default_pipeline();
        let local = pipeline.local_algorithms();
        assert!(local.contains(&"zlib"));
        assert!(local.contains(&"zlibx"));
        assert!(local.contains(&"none"));
    }

    #[test]
    fn capabilities_exchanged_remote_algorithms() {
        let exchanged = default_pipeline().exchange_capabilities(&["zstd", "zlib"]);
        let remote = exchanged.remote_algorithms();
        assert_eq!(remote, vec!["zstd", "zlib"]);
    }

    #[test]
    fn capabilities_exchanged_empty_remote() {
        let exchanged = default_pipeline().exchange_capabilities(&[]);
        let remote = exchanged.remote_algorithms();
        assert!(remote.is_empty());
    }

    #[test]
    fn algorithm_selected_name() {
        let selected = default_pipeline()
            .exchange_capabilities(&["zlib", "none"])
            .select_algorithm(false);
        assert_eq!(selected.selected_algorithm_name(), "zlib");
    }

    #[test]
    fn algorithm_selected_kind() {
        let selected = default_pipeline()
            .exchange_capabilities(&["zlib", "none"])
            .select_algorithm(false);
        assert_eq!(
            selected.selected_algorithm_kind(),
            Some(CompressionAlgorithmKind::Zlib)
        );
    }

    #[test]
    fn algorithm_selected_compression_level() {
        let pipeline = NegotiationPipeline::new(
            Box::new(DefaultCompressionNegotiator::new()),
            CompressionLevel::Best,
        );
        let selected = pipeline
            .exchange_capabilities(&["zlib"])
            .select_algorithm(false);
        assert_eq!(selected.compression_level(), CompressionLevel::Best);
    }

    #[test]
    fn server_respects_remote_order() {
        let selected = default_pipeline()
            .exchange_capabilities(&["none", "zlib"])
            .select_algorithm(true);
        // Server iterates remote list: "none" appears first and is supported
        assert_eq!(selected.selected_algorithm_name(), "none");
    }

    #[test]
    fn client_prefers_local_order() {
        let selected = default_pipeline()
            .exchange_capabilities(&["none", "zlibx"])
            .select_algorithm(false);
        // Client iterates local list: zlibx appears before "none" locally
        assert_eq!(selected.selected_algorithm_name(), "zlibx");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn full_lifecycle_selects_zstd_when_available() {
        let strategy = default_pipeline()
            .exchange_capabilities(&["zstd", "zlib", "none"])
            .select_algorithm(false)
            .into_strategy();
        assert_eq!(strategy.algorithm_name(), "zstd");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn algorithm_selected_kind_zstd() {
        let selected = default_pipeline()
            .exchange_capabilities(&["zstd", "zlib"])
            .select_algorithm(false);
        assert_eq!(
            selected.selected_algorithm_kind(),
            Some(CompressionAlgorithmKind::Zstd)
        );
    }

    #[test]
    fn strategy_from_pipeline_compresses_and_decompresses() {
        let strategy = default_pipeline()
            .exchange_capabilities(&["zlib", "none"])
            .select_algorithm(false)
            .into_strategy();

        let input = b"hello from the type-state pipeline";
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        strategy.compress(input, &mut compressed).unwrap();
        strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(&decompressed, input);
    }

    #[test]
    fn pipeline_states_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<NegotiationPipeline<Uninit>>();
        assert_send::<NegotiationPipeline<CapabilitiesExchanged>>();
        assert_send::<NegotiationPipeline<AlgorithmSelected>>();
    }

    // The following invalid transitions are prevented at compile time:
    //
    // ```compile_fail
    // use compress::strategy::type_state::NegotiationPipeline;
    // use compress::strategy::negotiator::DefaultCompressionNegotiator;
    // use compress::zlib::CompressionLevel;
    //
    // let pipeline = NegotiationPipeline::new(
    //     Box::new(DefaultCompressionNegotiator::new()),
    //     CompressionLevel::Default,
    // );
    // // ERROR: select_algorithm is not defined on NegotiationPipeline<Uninit>
    // pipeline.select_algorithm(false);
    // ```
    //
    // ```compile_fail
    // use compress::strategy::type_state::NegotiationPipeline;
    // use compress::strategy::negotiator::DefaultCompressionNegotiator;
    // use compress::zlib::CompressionLevel;
    //
    // let pipeline = NegotiationPipeline::new(
    //     Box::new(DefaultCompressionNegotiator::new()),
    //     CompressionLevel::Default,
    // );
    // // ERROR: into_strategy is not defined on NegotiationPipeline<Uninit>
    // pipeline.into_strategy();
    // ```
    //
    // ```compile_fail
    // use compress::strategy::type_state::NegotiationPipeline;
    // use compress::strategy::negotiator::DefaultCompressionNegotiator;
    // use compress::zlib::CompressionLevel;
    //
    // let pipeline = NegotiationPipeline::new(
    //     Box::new(DefaultCompressionNegotiator::new()),
    //     CompressionLevel::Default,
    // );
    // let exchanged = pipeline.exchange_capabilities(&["zlib"]);
    // // ERROR: into_strategy is not defined on NegotiationPipeline<CapabilitiesExchanged>
    // exchanged.into_strategy();
    // ```
    //
    // ```compile_fail
    // use compress::strategy::type_state::NegotiationPipeline;
    // use compress::strategy::negotiator::DefaultCompressionNegotiator;
    // use compress::zlib::CompressionLevel;
    //
    // let pipeline = NegotiationPipeline::new(
    //     Box::new(DefaultCompressionNegotiator::new()),
    //     CompressionLevel::Default,
    // );
    // let exchanged = pipeline.exchange_capabilities(&["zlib"]);
    // // ERROR: exchange_capabilities is not defined on
    // // NegotiationPipeline<CapabilitiesExchanged>
    // exchanged.exchange_capabilities(&["zstd"]);
    // ```
    //
    // ```compile_fail
    // use compress::strategy::type_state::NegotiationPipeline;
    // use compress::strategy::negotiator::DefaultCompressionNegotiator;
    // use compress::zlib::CompressionLevel;
    //
    // let pipeline = NegotiationPipeline::new(
    //     Box::new(DefaultCompressionNegotiator::new()),
    //     CompressionLevel::Default,
    // );
    // // Pipeline is consumed by exchange_capabilities - using it again is an error
    // let _exchanged = pipeline.exchange_capabilities(&["zlib"]);
    // let _again = pipeline.exchange_capabilities(&["zstd"]); // ERROR: use after move
    // ```

    #[test]
    fn consuming_transitions_prevent_reuse() {
        // This test verifies the consuming nature of transitions at runtime.
        // The pipeline moves through states and prior references are dropped.
        let pipeline = default_pipeline();
        let exchanged = pipeline.exchange_capabilities(&["zlib"]);
        // `pipeline` is now consumed - cannot be used
        let selected = exchanged.select_algorithm(false);
        // `exchanged` is now consumed - cannot be used
        let strategy = selected.into_strategy();
        // `selected` is now consumed - cannot be used
        assert_eq!(strategy.algorithm_name(), "zlib");
    }

    #[test]
    fn debug_formatting_uninit() {
        let pipeline = default_pipeline();
        let debug = format!("{pipeline:?}");
        assert!(debug.contains("NegotiationPipeline"));
    }

    #[test]
    fn debug_formatting_exchanged() {
        let exchanged = default_pipeline().exchange_capabilities(&["zlib"]);
        let debug = format!("{exchanged:?}");
        assert!(debug.contains("NegotiationPipeline"));
    }

    #[test]
    fn debug_formatting_selected() {
        let selected = default_pipeline()
            .exchange_capabilities(&["zlib"])
            .select_algorithm(false);
        let debug = format!("{selected:?}");
        assert!(debug.contains("NegotiationPipeline"));
    }

    #[test]
    fn pipeline_with_compression_level_fast() {
        let pipeline = NegotiationPipeline::new(
            Box::new(DefaultCompressionNegotiator::new()),
            CompressionLevel::Fast,
        );
        let strategy = pipeline
            .exchange_capabilities(&["zlib", "none"])
            .select_algorithm(false)
            .into_strategy();
        // Strategy should work regardless of level
        let mut compressed = Vec::new();
        strategy.compress(b"test data", &mut compressed).unwrap();
        assert!(!compressed.is_empty());
    }

    #[test]
    fn pipeline_with_empty_remote_produces_no_compression() {
        let strategy = default_pipeline()
            .exchange_capabilities(&[])
            .select_algorithm(false)
            .into_strategy();
        assert_eq!(strategy.algorithm_name(), "none");
    }

    #[test]
    fn pipeline_server_with_multiple_remote_algorithms() {
        let selected = default_pipeline()
            .exchange_capabilities(&["zlibx", "zlib", "none"])
            .select_algorithm(true);
        // Server iterates remote: zlibx maps to zlib kind which is supported
        // The name returned by the negotiator depends on from_name mapping
        let name = selected.selected_algorithm_name();
        assert!(name == "zlib" || name == "zlibx");
    }
}
