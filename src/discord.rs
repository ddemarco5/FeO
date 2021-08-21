// For sniffer post struct
use crate::reddit::SnifferPost;

// For Discord
use serenity::{
    model::{id::ChannelId},
    client::Client,
};


pub struct DiscordBot {
    serenity_bot: Client,
    chat_channel: ChannelId,
    archive_channel: ChannelId,
}

impl DiscordBot {
    pub async fn new(token: String, chat_channel: u64, archive_channel: u64) -> DiscordBot {
    info!("Created the discord bot");
    // Configure the client with your Discord bot token in the environment.
    let token = token;

    // Create a new instance of the Client, logging in as a bot. This will
    // automatically prepend your bot token with "Bot ", which is a requirement
    // by Discord for bot users.
    DiscordBot {
            serenity_bot: Client::builder(&token)
                .await
                .expect("Err creating client"),
            chat_channel: ChannelId(chat_channel), // main channel
            archive_channel: ChannelId(archive_channel), // the archive channel
        }
    }

    pub async fn post_message(&self, message: SnifferPost) {
        let http = &self.serenity_bot.cache_and_http.http;
        info!("Trying to send message: {}", message);
        let mut message_text = format!("{}\n{}\n> /r/{}", message.title, message.body, message.subreddit);
        self.chat_channel.say(&http, message_text.clone()).await.expect("Error sending message to main channel");
        // Append the post url to this one if we have it
        match message.url {
            Some(m) => {
                message_text.push_str(format!("\n{}", m).as_str());
            }
            None => {}
        }
        
        self.archive_channel.say(&http, message_text).await.expect("Error sending message to archive");
    }

}