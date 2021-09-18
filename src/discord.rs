// For sniffer post struct
use crate::reddit::SnifferPost;
use crate::Secrets;
use crate::player::AudioPlayer;

use std::sync::Arc;
use tokio::select;
use tokio::sync::{RwLock, Mutex};
use tokio_util::sync::CancellationToken;

// For Discord
use serenity::{
    model::{id::ChannelId},
    client::{Client, bridge::gateway::ShardManager},
};

// Enable songbird register trait for serenity
use songbird::SerenityInit;



pub struct DiscordBot {
    serenity_bot: Arc<RwLock<Client>>,
    bot_http: Arc<serenity::http::client::Http>,
    shard_handle: Option<futures_locks::Mutex<tokio::task::JoinHandle<()>>>,
    shard_cancel_token: CancellationToken,
    shard_manager: Arc<Mutex<ShardManager>>,
    chat_channel: ChannelId,
    test_channel: ChannelId,
    archive_channel: ChannelId,
    //songbird_instance: Option<Arc<Songbird>>,
    audio_player: Option<AudioPlayer>,
}

impl DiscordBot {
    //pub async fn new(token: String, chat_channel: u64, archive_channel: u64, test_channel: u64) -> DiscordBot {
    pub async fn new(secrets: Secrets) -> DiscordBot {
        info!("Created the discord bot");
        // Configure the client with your Discord bot token in the environment.
        let token = secrets.bot_token;

        // Create a songbird instance
        let audioplayer = AudioPlayer::new();
        warn!("Created audio player instance");
        let songbird_handle = audioplayer.get_songbird();

        // Create a new instance of the Client, logging in as a bot. This will
        // automatically prepend your bot token with "Bot ", which is a requirement
        // by Discord for bot users.
        let serenity_bot = Client::builder(&token)
            .event_handler(audioplayer.get_handler()) // Clone the audio player to keep ownership
            .register_songbird_with(songbird_handle.clone())
            .await
            .expect("Error creating client");
        /*
        {
            let mut data = serenity_bot.data.write().await;
            //data.insert::<MusicHandler>(handler_lock.clone())
            data.insert::<MusicHandler>(
                PlayerData::new(songbird_handle.clone())
            )
        }
        */
        // Pull our bot user id and initialize songbird with it
        let bot_user_id = serenity_bot.cache_and_http.http.get_current_user().await.expect("couldn't get current user").id;
        songbird_handle.initialise_client_data(1, bot_user_id);
        // Get a shared ref of our http cache so we can use it to send messages in an async fashion
        let http = serenity_bot.cache_and_http.http.clone();
        // And for shard manager too
        let manager_clone = serenity_bot.shard_manager.clone();
        let bot = DiscordBot {
                serenity_bot: Arc::new(RwLock::new(serenity_bot)),
                bot_http: http,
                shard_handle: None,
                shard_cancel_token: CancellationToken::new(),
                shard_manager: manager_clone,
                chat_channel: ChannelId(secrets.main_channel), // main channel
                test_channel: ChannelId(secrets.test_channel),
                archive_channel: ChannelId(secrets.archive_channel), // the archive channel
                //songbird_instance: Some(songbird_handle.clone()),
                audio_player: Some(audioplayer.clone()),
                //call_handle_lock: Some(handler_lock),
            };

        return bot;
    }

    pub async fn start_shards(&mut self, num_shards: u64) {
        let bot = self.serenity_bot.clone();
        let cloned_token = self.shard_cancel_token.clone();
        self.shard_handle = Some(futures_locks::Mutex::new(
            tokio::spawn(async move {
                let mut lock = bot.write().await;
                select! {
                    _ = lock.start_shards(num_shards) => {  
                        warn!("Shard threads stopped")
                    }
                    _ = cloned_token.cancelled() => {
                        warn!{"Cancelled our shards"}
                    }
                }
            })
        ));
        warn!("Started shards");
        
    }

    pub async fn print_shard_info(&self) {
        let lock = self.shard_manager.lock().await;
        let shard_runners = lock.runners.lock().await;
        for (id, runner) in shard_runners.iter() {
            warn!(
                "Shard ID {} is {} with a latency of {:?}",
                id, runner.stage, runner.latency,
            );
        }
    }

    pub async fn shutdown(mut self) {
        self.stop_audio().await;
        self.stop_shards().await; // we hold a write lock on serenity here, it's its run future
        //self.stop_audio().await;
    }

    //TODO: Find a way to make sure we can get the same instance of our original audio player
    async fn stop_audio(&self) {
        // If we have a player, hang up
        if let Some(mut player) = self.audio_player.clone() {
            player.hangup();
            // This is dumb as hell, but if we don't wait a little bit we'll remove the shards
            // before it has a chance to leave, they should really have a leave_blocking function
            // There's nothing we can poll to check to see if we've fully left either, the
            // associated structs reflect the state immediately

            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
    }

    async fn stop_shards(&mut self) {
        // Start the cancel
        self.shard_cancel_token.cancel();
        // Wait on our handle
        match &self.shard_handle{
            Some(x) => {
                let handle_lock = x.lock();
                handle_lock.await;
                warn!("Successfully waited on future");
                //handle_box.await.expect("failed waiting for the sharts to end");
                //*handle_lock.await;
            }
            None => {
                error!("We don't have a shard handle")
            }
        }
    }

    pub async fn post_message(&self, message: SnifferPost) {
        let http = &self.bot_http;
        info!("Trying to send message: {}", message);
        let mut message_text = message.discord_string();

        // Send message to our primary channel
        self.chat_channel.say(&http, message_text.clone()).await.expect("Error sending message to main channel");

        // Send message to our archive channel with url attached
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

impl Clone for DiscordBot {
    fn clone(&self) -> Self {
        DiscordBot {
            serenity_bot: self.serenity_bot.clone(),
            bot_http: self.bot_http.clone(),
            shard_handle: {
                match &self.shard_handle {
                    Some(h) => Some(h.clone()),
                    None => None,
                }
            },
            shard_cancel_token: self.shard_cancel_token.clone(),
            shard_manager: self.shard_manager.clone(),
            chat_channel: self.chat_channel.clone(),
            test_channel: self.test_channel.clone(),
            archive_channel: self.archive_channel.clone(),
            //songbird_instance: self.songbird_instance.clone(),
            //call_handle_lock: self.call_handle_lock.clone(),
            audio_player: self.audio_player.clone(),
        }
    }
}

