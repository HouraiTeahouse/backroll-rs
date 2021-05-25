pub struct SyncTestBackend;

impl SyncTestBackend {
    //     pub fn new()
    //    virtual GGPOErrorCode DoPoll(int timeout);
    //    virtual GGPOErrorCode AddPlayer(GGPOPlayer *player, GGPOPlayerHandle *handle);
    //    virtual GGPOErrorCode AddLocalInput(GGPOPlayerHandle player, void *values, int size);
    //    virtual GGPOErrorCode SyncInput(void *values, int size, int *disconnect_flags);
    //    virtual GGPOErrorCode IncrementFrame(void);
    //    virtual GGPOErrorCode Logv(char *fmt, va_list list);
}

//    SyncTestBackend(GGPOSessionCallbacks *cb, int frames, int num_players);
//    virtual ~SyncTestBackend();

// protected:
//    struct SavedInfo {
//       int         frame;
//       int         checksum;
//       char        *buf;
//       int         cbuf;
//       GameInput   input;
//    };

//    void RaiseSyncError(const char *fmt, ...);
//    void BeginLog(int saving);
//    void EndLog();
//    void LogSaveStates(SavedInfo &info);

// protected:
//    GGPOSessionCallbacks   _callbacks;
//    Sync                   _sync;
//    int                    _num_players;
//    int                    _check_distance;
//    int                    _last_verified;
//    bool                   _rollingback;
//    bool                   _running;
//    char                   _game[128];

//    GameInput                  _current_input;
//    GameInput                  _last_input;
//    RingBuffer<SavedInfo, 32>  _saved_frames;
// };
