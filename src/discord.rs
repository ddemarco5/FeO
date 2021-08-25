// For sniffer post struct
use crate::reddit::SnifferPost;
use crate::Secrets;

//use std::sync::{Arc,RwLock};
use std::sync::Arc;
use tokio::select;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

// For Discord
use serenity::{
    model::{id::ChannelId},
    client::Client,
    async_trait,
    prelude::*,
    model::{event::ResumedEvent, gateway::Ready}
};



pub struct DiscordBot {
    serenity_bot: Arc<RwLock<Client>>,
    bot_http: Arc<serenity::http::client::Http>,
    shard_handle: Option<tokio::task::JoinHandle<()>>,
    shard_cancel_token: CancellationToken,
    chat_channel: ChannelId,
    test_channel: ChannelId,
    archive_channel: ChannelId,
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        warn!("Connected as {}, setting bot to online", ready.user.name);
        ctx.reset_presence().await;
    }

    async fn resume(&self, ctx: Context, _: ResumedEvent) {
        warn!("Resumed (reconnected)");
        ctx.reset_presence().await;
    }
}

impl DiscordBot {
    //pub async fn new(token: String, chat_channel: u64, archive_channel: u64, test_channel: u64) -> DiscordBot {
    pub async fn new(secrets: Secrets) -> DiscordBot {
        info!("Created the discord bot");
        // Configure the client with your Discord bot token in the environment.
        let token = secrets.bot_token;

        // Create a new instance of the Client, logging in as a bot. This will
        // automatically prepend your bot token with "Bot ", which is a requirement
        // by Discord for bot users.
        let serenity_bot = Client::builder(&token)
            .event_handler(Handler)
            .await
            .expect("Err creating client");
        // Get a shared ref of our http cache so we can use it to send messages in an async fashion
        let http = serenity_bot.cache_and_http.http.clone();
        let bot = DiscordBot {
                serenity_bot: Arc::new(RwLock::new(serenity_bot)),
                bot_http: http,
                shard_handle: None,
                shard_cancel_token: CancellationToken::new(),
                chat_channel: ChannelId(secrets.main_channel), // main channel
                test_channel: ChannelId(secrets.test_channel),
                archive_channel: ChannelId(secrets.archive_channel), // the archive channel
            };
        // bot.serenity_bot.start().await.expect("Failed to start shard")
        // Grab the discord object
        /*
        let discord_bot = bot.serenity_bot.clone();
        tokio::spawn(async move{
            //bot.serenity_bot.start_shard(0, 1).await.expect("Failed to start shard 0 of 1 ");
            discord_bot.read().unwrap().start_shard(0, 1).await.expect("Failed to start shard 0 of 1 ");
        });
        */

        return bot;
    }

    pub async fn start_shards(&mut self, num_shards: u64) {
        let bot = self.serenity_bot.clone();
        let cloned_token = self.shard_cancel_token.clone();
        self.shard_handle = Some(
            tokio::spawn(async move {
                let mut lock = bot.write().await;
                select! {
                    //_ = lock.start_shards(num_shards).await.expect("Unable to start shards") => {
                    _ = lock.start_shards(num_shards) => {
                        warn!("Shart threads stopped")
                    }
                    _ = cloned_token.cancelled() => {
                        warn!{"Cancelled our sharts"}
                    }
                }
            })
        );
        warn!("Started sharts");
    }

    pub async fn stop_shards(&mut self) {
        self.shard_cancel_token.cancel();
        match self.shard_handle.as_mut() {
            Some(x) => {
                x.await.expect("failed waiting for the sharts to end");
            }
            None => {
                error!("We don't have a shard handle")
            }
        }
    }

    pub async fn post_message(&self, message: SnifferPost) {
        //let http_lock = self.serenity_bot.read().await;
        //let http = &self.serenity_bot.cache_and_http.http;
        //let http = &http_lock.cache_and_http.http;
        let http = &self.bot_http;
        info!("Trying to send message: {}", message);
        let mut message_text = format!("{}\n{}\n> /r/{}", message.title, message.body, message.subreddit);
        self.chat_channel.say(&http, message_text.clone()).await.expect("Error sending message to main channel");
        // Append the post url to this one if we have it
        match message.url { 
            Some(m) => {
                message_text.push_str(format!("\n<{}>", m).as_str());
            }
            None => {}
        }
        
        self.archive_channel.say(&http, message_text).await.expect("Error sending message to archive");
    }

    #[allow(dead_code)]
    pub async fn post_debug_string(&self, message: String) {
        let http = &self.bot_http;
        warn!("Trying to send debug message");
        self.test_channel.say(&http, message.clone()).await.expect("Error sending test message");
    }

}