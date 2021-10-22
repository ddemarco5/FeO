#[macro_use]
extern crate lazy_static;

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

mod reddit;
mod discord;
mod player;

#[derive(Deserialize, Debug, Clone)]
pub struct Secrets {
    bot_token: String,
    guild_id: u64,
    main_channel: u64,
    audio_channel: u64,
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


    let mut discord_bot = discord::DiscordBot::new(secrets).await;
    discord_bot.start_shards(1).await;
    

    // Clone discord bot to use in a thread
    let discord_bot_clone = discord_bot.clone();
    // Run in a loop to wait for the sniffer to strike again
    let run_token = tokio::spawn(async move {
        warn!("Starting scraper thread");
        loop {
            // Check every X seconds
            sleep(Duration::from_secs(45)).await;
            match reddit.update() {
                Ok(message_opt) => {
                    match message_opt {
                        Some(messages) => {
                            warn!("Got {} new messages", messages.len());
                            //let lock = discord_bot_clone.read().await;
                            for message in messages {
                                warn!("New sniffer message!:\n{}", message);
                                //lock.post_message(message).await;
                                discord_bot_clone.post_message(message).await;
                            }    
                        },
                        None => {
                            debug!("No new sniffer message");
                        },
                    }
                }
                Err(error) => {
                    error!("Encountered an error\n{}\nskipping this loop", error);
                }
            }
        }
    });

    // Clone discord bot to use in a thread
    let discord_bot_clone = discord_bot.clone();
    let future_wait = tokio::spawn(async move {
        select! {
            _ = wait_token(run_token) => {
                warn!("Bot thread stopped")
            }
            _ = wait_sigint() => {
                warn!("Got SIGINT");
                // Kill our shards
                //discord_bot_clone.write().await.stop_shards().await;
                discord_bot_clone.shutdown().await;
            }
        };
    });


    //discord_bot.clone().read().await.print_shard_info().await;
    discord_bot.print_shard_info().await;

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