use clap::{App, Arg, ArgMatches, SubCommand};
use cypher_client::cache_account;
use cypher_utils::{
    accounts_cache::AccountsCache, contexts::CypherContext, services::StreamingAccountInfoService,
};
use log::{info, warn};
use std::{error, sync::Arc};
use tokio::sync::broadcast::channel;

use crate::{
    cli::CliError,
    common::context::{builder::ContextBuilder, GlobalContext},
    config::{get_user_info, Config, PersistentConfig},
    context::builders::global::GlobalContextBuilder,
    liquidator::{
        accounts::CypherAccountsService, config::LiquidatorConfig, error::Error, Liquidator,
    },
};

use super::{command::CliCommand, CliConfig, CliResult};

pub trait LiquidatorSubCommands {
    fn liquidator_subcommands(self) -> Self;
}

impl LiquidatorSubCommands for App<'_, '_> {
    fn liquidator_subcommands(self) -> Self {
        self.subcommand(
            SubCommand::with_name("liquidator")
            .about("Runs a liquidator bot.")
            .subcommand(
                SubCommand::with_name("run").about("Runs the liquidator.")
                .arg(
                    Arg::with_name("config")
                        .short("c")
                        .long("config")
                        .value_name("FILE")
                        .global(true)
                        .takes_value(true)
                        .help("Filepath to a config. This config should follow the format displayed in `/cfg/liquidator/default.json`."),
                )
            )
        )
    }
}

pub fn parse_liquidator_command(matches: &ArgMatches) -> Result<CliCommand, Box<dyn error::Error>> {
    match matches.subcommand() {
        ("run", Some(matches)) => {
            let config_path = match matches.value_of("config") {
                Some(s) => s,
                None => {
                    return Err(Box::new(CliError::BadParameters(
                        "Path to config path not provided.".to_string(),
                    )));
                }
            };
            Ok(CliCommand::Liquidator {
                config_path: config_path.to_string(),
            })
        }
        ("", None) => {
            eprintln!("{}", matches.usage());
            Err(Box::new(CliError::CommandNotRecognized(
                "no subcommand given".to_string(),
            )))
        }
        _ => unreachable!(),
    }
}

pub async fn process_liquidator_command(
    config: &CliConfig,
    config_path: &str,
) -> Result<CliResult, Box<dyn error::Error>> {
    env_logger::init();
    let rpc_client = config.rpc_client.as_ref().unwrap();
    let pubsub_client = config.pubsub_client.as_ref().unwrap();
    let keypair = config.keypair.as_ref().unwrap();

    info!("Setting up components from config..");

    let shutdown_sender = Arc::new(channel::<bool>(1).0);

    let mm_config: Config<LiquidatorConfig> = match PersistentConfig::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            warn!("There was an error loading config: {}", e.to_string());
            return Err(Box::new(CliError::BadParameters(
                "Failed to load liquidator config.".to_string(),
            )));
        }
    };

    let _cypher_ctx = match CypherContext::load(rpc_client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            warn!(
                "There was an error loading the cypher context: {}",
                e.to_string()
            );
            return Err(Box::new(CliError::ContextError(e)));
        }
    };

    let user_info =
        match get_user_info::<LiquidatorConfig>(rpc_client.clone(), &mm_config, keypair).await {
            Ok(ui) => ui,
            Err(e) => {
                warn!("There was an error getting user info: {}", e.to_string());
                return Err(Box::new(CliError::Liquidator(Error::ClientError(e))));
            }
        };

    let accounts_cache = Arc::new(AccountsCache::new());

    let global_context_builder: Arc<dyn ContextBuilder<Output = GlobalContext> + Send> =
        Arc::new(GlobalContextBuilder::new(
            accounts_cache.clone(),
            shutdown_sender.clone(),
            user_info.master_account.clone(),
            user_info.sub_account.clone(),
        ));

    // at this point we have prepared everything we need
    // all that is left is spawning tasks for the components that should run concurrently in order to propagate data
    info!("🔥 Let's ride! 🔥");

    let global_context_builder_clone = global_context_builder.clone();
    let _global_context_handler = tokio::spawn(async move {
        match global_context_builder_clone.start().await {
            Ok(_) => (),
            Err(_e) => {
                warn!("There was an error running the Global Context Builder.")
            }
        }
    });

    // only add the necessary subscriptions to the service so the initial account fetching propagates to all listeners
    let streaming_account_service = Arc::new(StreamingAccountInfoService::new(
        accounts_cache.clone(),
        pubsub_client.clone(),
        rpc_client.clone(),
        shutdown_sender.subscribe(),
        &vec![],
    ));
    let streaming_account_service_clone = streaming_account_service.clone();
    let streaming_account_service_handle = tokio::spawn(async move {
        streaming_account_service_clone.start_service().await;
    });

    // fetch all cypher accounts
    // - create cache of pubkeys, periodically update it
    // fetch all cypher sub accounts
    // - create cache of pubkeys, periodically update it

    // start a service which fetches all cypher accounts and sub accounts asynchronously
    // and then sends a message for the service above to subscribe to their account datas
    let cypher_accounts_service = Arc::new(CypherAccountsService::new(
        rpc_client.clone(),
        streaming_account_service.clone(),
        accounts_cache.clone(),
        shutdown_sender.clone(),
    ));
    let cypher_accounts_service_clone = cypher_accounts_service.clone();
    let cypher_accounts_service_handle = tokio::spawn(async move {
        cypher_accounts_service_clone.start().await;
    });

    // fetch all clearings
    let liquidator = Arc::new(Liquidator::new(
        rpc_client.clone(),
        global_context_builder.clone(),
        cypher_accounts_service.update_sender.clone(),
        shutdown_sender.clone(),
        user_info.clone(),
    ));
    let liquidator_clone = liquidator.clone();
    let liquidator_handle = tokio::spawn(async move { liquidator_clone.start().await });

    let accounts = vec![
        cache_account::id(),
        user_info.master_account,
        user_info.sub_account,
    ];
    streaming_account_service.add_subscriptions(&accounts).await;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            match shutdown_sender.send(true) {
                Ok(_) => {
                    info!("Sucessfully sent shutdown signal. Waiting for tasks to complete...")
                },
                Err(e) => {
                    warn!("Failed to send shutdown error: {}", e.to_string());
                }
            };
        },
    }

    tokio::join!(
        liquidator_handle,
        streaming_account_service_handle,
        cypher_accounts_service_handle
    );

    Ok(CliResult {})
}
