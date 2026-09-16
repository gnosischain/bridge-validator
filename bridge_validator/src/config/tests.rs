#[cfg(test)]
mod tests {
    use crate::config::{BlockProcessingMode, Config};

    // Serializes tests that mutate process-global env vars to prevent races.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn test_parse_single_rpc_url() {
        let result = Config::parse_rpc_urls("https://eth.example.com".to_string());
        assert_eq!(result, vec!["https://eth.example.com"]);
    }

    #[test]
    fn test_parse_multiple_rpc_urls() {
        let result = Config::parse_rpc_urls(
            "https://eth1.example.com,https://eth2.example.com,https://eth3.example.com"
                .to_string(),
        );
        assert_eq!(
            result,
            vec![
                "https://eth1.example.com",
                "https://eth2.example.com",
                "https://eth3.example.com"
            ]
        );
    }

    #[test]
    fn test_parse_rpc_urls_with_spaces() {
        let result = Config::parse_rpc_urls(
            "https://eth1.example.com, https://eth2.example.com , https://eth3.example.com"
                .to_string(),
        );
        assert_eq!(
            result,
            vec![
                "https://eth1.example.com",
                "https://eth2.example.com",
                "https://eth3.example.com"
            ]
        );
    }

    #[test]
    fn test_parse_rpc_urls_filters_empty() {
        let result = Config::parse_rpc_urls(
            "https://eth1.example.com,,https://eth2.example.com".to_string(),
        );
        assert_eq!(
            result,
            vec!["https://eth1.example.com", "https://eth2.example.com"]
        );
    }

    #[test]
    fn test_config_default_poll_interval() {
        let _lock = env_lock();
        // Clear environment to ensure test isolation
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");

        // Set only required env vars
        std::env::set_var("ETH_RPC", "https://eth.example.com");
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");

        let config = Config::from_env().unwrap();

        // Should use default value of 10 seconds
        assert_eq!(config.poll_interval_secs, 10);
        // Verify single RPC is parsed correctly
        assert_eq!(config.eth_rpc, vec!["https://eth.example.com"]);
    }

    #[test]
    fn test_config_multiple_rpc_endpoints() {
        let _lock = env_lock();
        // Clear environment to ensure test isolation
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");

        std::env::set_var(
            "ETH_RPC",
            "https://eth1.example.com,https://eth2.example.com,https://eth3.example.com",
        );
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");

        let config = Config::from_env().unwrap();

        assert_eq!(config.eth_rpc.len(), 3);
        assert_eq!(config.eth_rpc[0], "https://eth1.example.com");
        assert_eq!(config.eth_rpc[1], "https://eth2.example.com");
        assert_eq!(config.eth_rpc[2], "https://eth3.example.com");
    }

    #[test]
    fn test_config_custom_poll_interval() {
        let _lock = env_lock();
        // Clear environment to ensure test isolation
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");

        std::env::set_var("ETH_RPC", "https://eth.example.com");
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");
        std::env::set_var("POLL_INTERVAL_SECS", "30");

        let config = Config::from_env().unwrap();

        assert_eq!(config.poll_interval_secs, 30);

        // Cleanup
        std::env::remove_var("POLL_INTERVAL_SECS");
    }

    #[test]
    fn test_config_invalid_poll_interval_uses_default() {
        let _lock = env_lock();

        // A malformed value must land on the same default as an unset one —
        // otherwise the cadence appears nowhere in the operator's config.
        for bad in ["invalid", "0", "-5", "  "] {
            // Clear environment to ensure test isolation
            std::env::remove_var("POLL_INTERVAL_SECS");
            std::env::remove_var("MAX_RETRY_COUNT");

            std::env::set_var("ETH_RPC", "https://eth.example.com");
            std::env::set_var("GC_RPC", "https://gc.example.com");
            std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
            std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");
            std::env::set_var("POLL_INTERVAL_SECS", bad);

            let config = Config::from_env().unwrap();

            assert_eq!(
                config.poll_interval_secs,
                crate::config::DEFAULT_POLL_INTERVAL_SECS,
                "POLL_INTERVAL_SECS='{}' should fall back to the default",
                bad
            );
            assert_eq!(config.poll_interval_secs, 10);
        }

        // Cleanup
        std::env::remove_var("POLL_INTERVAL_SECS");
    }

    #[test]
    fn test_config_default_max_retry_count() {
        let _lock = env_lock();
        set_required_env_vars();
        std::env::remove_var("MAX_RETRY_COUNT");

        let config = Config::from_env().unwrap();

        assert_eq!(
            config.max_retry_count,
            crate::config::DEFAULT_MAX_RETRY_COUNT
        );
        assert_eq!(config.max_retry_count, 5);
    }

    #[test]
    fn test_config_custom_max_retry_count() {
        let _lock = env_lock();
        set_required_env_vars();
        std::env::set_var("MAX_RETRY_COUNT", "12");

        let config = Config::from_env().unwrap();

        assert_eq!(config.max_retry_count, 12);

        // Cleanup
        std::env::remove_var("MAX_RETRY_COUNT");
    }

    #[test]
    fn test_config_invalid_max_retry_count_uses_default() {
        let _lock = env_lock();

        // Zero would stall the pipeline outright — no row would ever be
        // claimed — so it is rejected the same way an unparseable value is.
        for bad in ["invalid", "0", "-5", "  "] {
            set_required_env_vars();
            std::env::set_var("MAX_RETRY_COUNT", bad);

            let config = Config::from_env().unwrap();

            assert_eq!(
                config.max_retry_count,
                crate::config::DEFAULT_MAX_RETRY_COUNT,
                "MAX_RETRY_COUNT='{}' should fall back to the default",
                bad
            );
        }

        // Cleanup
        std::env::remove_var("MAX_RETRY_COUNT");
    }

    #[test]
    fn test_config_default_max_block_range() {
        let _lock = env_lock();
        set_required_env_vars();
        std::env::remove_var("MAX_BLOCK_RANGE");

        let config = Config::from_env().unwrap();

        assert_eq!(
            config.max_block_range,
            crate::config::DEFAULT_MAX_BLOCK_RANGE
        );
        assert_eq!(config.max_block_range, 2000);
    }

    #[test]
    fn test_config_custom_max_block_range() {
        let _lock = env_lock();
        set_required_env_vars();
        std::env::set_var("MAX_BLOCK_RANGE", "500");

        let config = Config::from_env().unwrap();

        assert_eq!(config.max_block_range, 500);

        // Cleanup
        std::env::remove_var("MAX_BLOCK_RANGE");
    }

    #[test]
    fn test_config_invalid_max_block_range_uses_default() {
        let _lock = env_lock();

        // Zero would make the indexer's chunk cursor advance by nothing, so it
        // is rejected the same way an unparseable value is.
        for bad in ["invalid", "0", "-5", "  "] {
            set_required_env_vars();
            std::env::set_var("MAX_BLOCK_RANGE", bad);

            let config = Config::from_env().unwrap();

            assert_eq!(
                config.max_block_range,
                crate::config::DEFAULT_MAX_BLOCK_RANGE,
                "MAX_BLOCK_RANGE='{}' should fall back to the default",
                bad
            );
        }

        // Cleanup
        std::env::remove_var("MAX_BLOCK_RANGE");
    }

    /// Set the env vars `Config::from_env` requires, and clear the fcr check
    /// interval so each case below starts from a known state.
    fn set_required_env_vars() {
        std::env::remove_var("FCR_CHECK_INTERVAL_SECS");
        std::env::set_var("ETH_RPC", "https://eth.example.com");
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");
    }

    #[test]
    fn test_config_default_fcr_check_interval() {
        let _lock = env_lock();
        set_required_env_vars();

        let config = Config::from_env().unwrap();

        assert_eq!(
            config.fcr_check_interval_secs,
            crate::config::DEFAULT_FCR_CHECK_INTERVAL_SECS
        );
        assert_eq!(config.fcr_check_interval_secs, 30);
    }

    #[test]
    fn test_config_custom_fcr_check_interval() {
        let _lock = env_lock();
        set_required_env_vars();
        std::env::set_var("FCR_CHECK_INTERVAL_SECS", "5");

        let config = Config::from_env().unwrap();

        assert_eq!(config.fcr_check_interval_secs, 5);

        // Cleanup
        std::env::remove_var("FCR_CHECK_INTERVAL_SECS");
    }

    #[test]
    fn test_config_invalid_fcr_check_interval_uses_default() {
        let _lock = env_lock();

        // Zero would turn the revalidation loop into a hot loop, so it is
        // rejected the same way an unparseable value is.
        for bad in ["invalid", "0", "-5", "  "] {
            set_required_env_vars();
            std::env::set_var("FCR_CHECK_INTERVAL_SECS", bad);

            let config = Config::from_env().unwrap();

            assert_eq!(
                config.fcr_check_interval_secs,
                crate::config::DEFAULT_FCR_CHECK_INTERVAL_SECS,
                "FCR_CHECK_INTERVAL_SECS='{}' should fall back to the default",
                bad
            );
        }

        // Cleanup
        std::env::remove_var("FCR_CHECK_INTERVAL_SECS");
    }

    #[test]
    fn test_config_default_bridge_addresses() {
        let _lock = env_lock();
        // Clear environment to ensure test isolation
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");

        std::env::set_var("ETH_RPC", "https://eth.example.com");
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");

        // Remove all bridge address env vars to test defaults
        std::env::remove_var("ETH_AMB_BRIDGE_ADDRESS");
        std::env::remove_var("GC_AMB_BRIDGE_ADDRESS");
        std::env::remove_var("ETH_XDAI_BRIDGE_ADDRESS");
        std::env::remove_var("GC_XDAI_BRIDGE_ADDRESS");

        let config = Config::from_env().unwrap();

        // Verify default addresses are used
        assert_eq!(
            format!("{:?}", config.eth_amb_bridge_address),
            "0x4c36d2919e407f0cc2ee3c993ccf8ac26d9ce64e"
        );
        assert_eq!(
            format!("{:?}", config.gc_amb_bridge_address),
            "0x75df5af045d91108662d8080fd1fefad6aa0bb59"
        );
    }

    #[test]
    fn test_config_missing_required_rpc() {
        let _lock = env_lock();
        // Clear environment to ensure test isolation
        std::env::remove_var("ETH_RPC");
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");

        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");

        let result = Config::from_env();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("ETH_RPC"));
    }

    #[test]
    fn test_config_optional_private_keys() {
        let _lock = env_lock();
        // Clear environment to ensure test isolation
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");
        std::env::remove_var("XDAI_VALIDATOR_PRIV_KEY");
        std::env::remove_var("AMB_VALIDATOR_PRIV_KEY");

        std::env::set_var("ETH_RPC", "https://eth.example.com");
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");

        let config = Config::from_env().unwrap();

        // Private keys should be None when not set
        assert!(config.xdai_validator_private_key.is_none());
        assert!(config.amb_validator_private_key.is_none());
    }

    /// Sets the env vars every `from_env` test needs and clears the two mode
    /// vars so a leaked value from a previous test can't decide the outcome.
    fn set_base_env() {
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");
        std::env::remove_var("ETH_BLOCK_PROCESSING_MODE");
        std::env::remove_var("GC_BLOCK_PROCESSING_MODE");

        std::env::set_var("ETH_RPC", "https://eth.example.com");
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");
    }

    // The two chains do not share a default: ETH runs fcr so ETH->GC relaying is
    // not gated on ~12.8m finality, GC stays conservative because that is the
    // direction where this validator's signature is an irreversible commitment.
    #[test]
    fn test_block_processing_mode_defaults_are_per_chain() {
        let _lock = env_lock();
        set_base_env();

        let config = Config::from_env().unwrap();

        assert_eq!(config.eth_block_processing_mode, BlockProcessingMode::Fcr);
        assert_eq!(
            config.gc_block_processing_mode,
            BlockProcessingMode::BlockFinality
        );

        // Only the fcr chain is handed to the checker.
        let fcr_chains = config.fcr_chains();
        assert_eq!(fcr_chains.len(), 1);
        assert_eq!(fcr_chains[0].0, "eth");
    }

    #[test]
    fn test_block_processing_mode_is_per_chain() {
        let _lock = env_lock();
        set_base_env();
        std::env::set_var("ETH_BLOCK_PROCESSING_MODE", "fcr");
        std::env::set_var("GC_BLOCK_PROCESSING_MODE", "block-finality");

        let config = Config::from_env().unwrap();

        assert_eq!(config.mode_for_chain("eth"), BlockProcessingMode::Fcr);
        assert_eq!(
            config.mode_for_chain("gc"),
            BlockProcessingMode::BlockFinality
        );

        // Only the fcr chain is handed to the checker, with its EL RPC array.
        let fcr_chains = config.fcr_chains();
        assert_eq!(fcr_chains.len(), 1);
        assert_eq!(fcr_chains[0].0, "eth");
        assert_eq!(fcr_chains[0].1, ["https://eth.example.com"]);

        std::env::remove_var("ETH_BLOCK_PROCESSING_MODE");
        std::env::remove_var("GC_BLOCK_PROCESSING_MODE");
    }

    #[test]
    fn test_block_processing_mode_parsing_is_case_insensitive() {
        let _lock = env_lock();
        set_base_env();
        std::env::set_var("ETH_BLOCK_PROCESSING_MODE", " FCR ");
        std::env::set_var("GC_BLOCK_PROCESSING_MODE", "Block-Finality");

        let config = Config::from_env().unwrap();

        assert_eq!(config.eth_block_processing_mode, BlockProcessingMode::Fcr);
        assert_eq!(
            config.gc_block_processing_mode,
            BlockProcessingMode::BlockFinality
        );

        std::env::remove_var("ETH_BLOCK_PROCESSING_MODE");
        std::env::remove_var("GC_BLOCK_PROCESSING_MODE");
    }

    // A typo must not quietly hand back block-finality: the operator would
    // believe fcr is on and never find out.
    #[test]
    fn test_invalid_block_processing_mode_is_rejected() {
        let _lock = env_lock();
        set_base_env();
        std::env::set_var("ETH_BLOCK_PROCESSING_MODE", "fast");

        let result = Config::from_env();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("ETH_BLOCK_PROCESSING_MODE"), "got: {}", err);

        std::env::remove_var("ETH_BLOCK_PROCESSING_MODE");
    }

    // An empty value is the same as not setting the var (the .env.example
    // ships the key with no value).
    #[test]
    fn test_empty_block_processing_mode_uses_default() {
        let _lock = env_lock();
        set_base_env();
        std::env::set_var("ETH_BLOCK_PROCESSING_MODE", "");
        std::env::set_var("GC_BLOCK_PROCESSING_MODE", "");

        let config = Config::from_env().unwrap();

        // Empty must land on each chain's own default, not on a shared one.
        assert_eq!(config.eth_block_processing_mode, BlockProcessingMode::Fcr);
        assert_eq!(
            config.gc_block_processing_mode,
            BlockProcessingMode::BlockFinality
        );

        std::env::remove_var("ETH_BLOCK_PROCESSING_MODE");
        std::env::remove_var("GC_BLOCK_PROCESSING_MODE");
    }

    #[test]
    fn test_bridge_modes_for_chain() {
        assert_eq!(
            Config::bridge_modes_for_chain("eth"),
            ["AMB_ETH", "XDAI_ETH"]
        );
        assert_eq!(Config::bridge_modes_for_chain("gc"), ["AMB_GC", "XDAI_GC"]);
    }

    #[test]
    fn test_set_mode_for_chain_downgrades_only_that_chain() {
        let _lock = env_lock();
        set_base_env();
        std::env::set_var("ETH_BLOCK_PROCESSING_MODE", "fcr");
        std::env::set_var("GC_BLOCK_PROCESSING_MODE", "fcr");

        let mut config = Config::from_env().unwrap();
        config.set_mode_for_chain("eth", BlockProcessingMode::BlockFinality);

        assert_eq!(
            config.mode_for_chain("eth"),
            BlockProcessingMode::BlockFinality
        );
        assert_eq!(config.mode_for_chain("gc"), BlockProcessingMode::Fcr);

        std::env::remove_var("ETH_BLOCK_PROCESSING_MODE");
        std::env::remove_var("GC_BLOCK_PROCESSING_MODE");
    }

    #[test]
    fn test_config_empty_rpc_urls() {
        let _lock = env_lock();
        // Clear environment to ensure test isolation
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");

        std::env::set_var("ETH_RPC", "");
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");

        let result = Config::from_env();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("ETH_RPC must contain at least one valid URL"));
    }
}
