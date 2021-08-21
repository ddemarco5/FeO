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

#[derive(Deserialize, Debug)]
struct Secrets {
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
        .level("debug")
        .output_file()
        .output_console()
        .build();

    simple_log::new(config).expect("Error building log file");

    // Load our secrets
    let file = OpenOptions::new().read(true).open("secrets.yaml").expect("Couldn't load secrets file, exiting");

    let secrets: Secrets = serde_yaml::from_reader(file).expect("Serde error deserializing secrets");
    debug!("{:?}", secrets);

    // Create our api interfaces
    let mut reddit = reddit::RedditScraper::new(secrets.sniffer);
    let discord_bot = discord::DiscordBot::new(secrets.bot_token, secrets.main_channel, secrets.archive_channel).await;


    // Update the reddit posts so we know when a new one kicks in
    reddit.update().await.expect("Error doing the initial reddit update");

    // Run in a loop to wait for the sniffer to strike again
    let run_token = tokio::spawn(async move {
        debug!("Started the bot loop");
        loop {
            // Wait 5 mins (300 seconds)
            sleep(Duration::from_secs(30)).await;
            match reddit.update().await {
                Ok(message_opt) => {
                    match message_opt {
                        Some(messages) => {
                            info!("Got {} new messages", messages.len());
                            for message in messages {
                                info!("New sniffer message!:\n{}", message);
                                discord_bot.post_message(message).await;
                            }    
                        },
                        None => {
                            debug!("No sniffer message");
                        },
                    }
                }
                Err(error) => {
                    error!("Encountered an error\n{}\nskipping this loop", error);
                }
            }
        }
    });

    let future_wait = tokio::spawn(async move {
        select! {
            _ = wait_token(run_token) => {
                debug!("Our loop finished first")
            }
            _ = wait_sigint() => {
                debug!("Our control-c came first")
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