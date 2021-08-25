use tokio::{
    signal,
    time::sleep,
    select,
};

use std::time::Duration;

#[macro_use]
extern crate log;
use simple_log::LogConfigBuilder;
use serde::Deserialize;
use std::fs::OpenOptions;

use std::sync::Arc;
use tokio::sync::RwLock;

mod reddit;
mod discord;

#[derive(Deserialize, Debug, Clone)]
pub struct Secrets {
    bot_token: String,
    main_channel: u64,
    test_channel: u64,
    archive_channel: u64,
    sniffer: String
}

#[tokio::main]
async fn main() {

    // Create our log file
    let config = LogConfigBuilder::builder()
        .path("./sniffer_log.txt")
        .size(500)
        .roll_count(10)
        .level("warn")
        .output_file()
        .output_console()
        .build();

    simple_log::new(config).expect("Error building log file");

    // Load our secrets
    let file = OpenOptions::new().read(true).open("secrets.yaml").expect("Couldn't load secrets file, exiting");
    warn!("Secrets loaded");

    let secrets: Secrets = serde_yaml::from_reader(file).expect("Serde error deserializing secrets");
    debug!("{:?}", secrets.clone());

    // Create our api interfaces
    let mut reddit = reddit::RedditScraper::new(secrets.sniffer.clone());
    let discord_bot = Arc::new(RwLock::new(
        //discord::DiscordBot::new(secrets.bot_token, secrets.main_channel, secrets.archive_channel, secrets.test_channel).await
        discord::DiscordBot::new(secrets).await
    ));

    // Start our discord shard
    { // closure so we drop the lock after we're done
        let mut lock = discord_bot.write().await;
        lock.start_shards(1).await;
    }
    


    // Update the reddit posts so we know when a new one kicks in
    reddit.update().await.expect("Error doing the initial reddit update");
    warn!("Pulled intial posts");

    let discord_bot_clone = discord_bot.clone();
    // Run in a loop to wait for the sniffer to strike again
    let run_token = tokio::spawn(async move {
        warn!("Starting scraper thread");
        loop {
            // Check every X seconds
            sleep(Duration::from_secs(60)).await;
            match reddit.update().await {
                Ok(message_opt) => {
                    match message_opt {
                        Some(messages) => {
                            warn!("Got {} new messages", messages.len());
                            let lock = discord_bot_clone.read().await;
                            for message in messages {
                                warn!("New sniffer message!:\n{}", message);
                                lock.post_message(message).await;
                            }    
                        },
                        None => {
                            warn!("No sniffer message");
                        },
                    }
                }
                Err(error) => {
                    error!("Encountered an error\n{}\nskipping this loop", error);
                }
            }
        }
    });
    let discord_bot_clone = discord_bot.clone();
    let future_wait = tokio::spawn(async move {
        select! {
            _ = wait_token(run_token) => {
                warn!("Bot thread stopped")
            }
            _ = wait_sigint() => {
                warn!("Got SIGINT");
                // Kill our sharts
                discord_bot_clone.write().await.stop_shards().await;
            }
        };
    });

    println!("Ctrl-C to exit...");
    
    
    future_wait.await.expect("failed to wait for run token");
    
    println!("Gooby!");

}

async fn wait_token<T>(handle: tokio::task::JoinHandle<T>) {
    handle.await.unwrap();
}
async fn wait_sigint() {
    signal::ctrl_c().await.unwrap()
}