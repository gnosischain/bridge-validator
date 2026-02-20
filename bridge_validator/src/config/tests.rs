#[cfg(test)]
mod tests {
    use crate::config::Config;
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
        // Clear environment to ensure test isolation
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");

        std::env::set_var("ETH_RPC", "https://eth.example.com");
        std::env::set_var("GC_RPC", "https://gc.example.com");
        std::env::set_var("ETH_BC_RPC", "https://eth-beacon.example.com");
        std::env::set_var("GC_BC_RPC", "https://gc-beacon.example.com");
        std::env::set_var("POLL_INTERVAL_SECS", "invalid");

        let config = Config::from_env().unwrap();

        // Should fall back to default value of 5 when parse fails
        assert_eq!(config.poll_interval_secs, 5);

        // Cleanup
        std::env::remove_var("POLL_INTERVAL_SECS");
    }

    #[test]
    fn test_config_default_bridge_addresses() {
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

    #[test]
    fn test_config_empty_rpc_urls() {
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

    #[test]
    fn test_config_only_commas() {
        // Clear environment to ensure test isolation
        std::env::remove_var("POLL_INTERVAL_SECS");
        std::env::remove_var("MAX_RETRY_COUNT");

        std::env::set_var("ETH_RPC", ",,,");
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
