// A subset for the draft for the Tournoi Francophone Saison 5 (https://midos.house/event/fr/5), not including hard settings or MQ.
// This removes most of the complexities of the draft format, such as the mutual exclusion of keysy and keysanity, or the extra yes/no question for mixed dungeon ER.

groups: {
    "Settings classiques": {
        camc: {
            default: "on",
            "off",
        },
        start_weirdegg: {
            default: "random-skip",
            "vanilla-shuffle",
        },
        keysy: {
            default: "off",
            "on",
        },
        deku: {
            default: "closed",
            "open",
        },
        card: {
            default: "vanilla",
            "shuffle",
        },
        ocarina: {
            default: "startwith",
            "shuffle",
        },
        chubags: {
            default: "off",
            "on",
        },
        dungeon_er: {
            default: "off",
            "on",
        },
        songs: {
            default: "songs",
            "anywhere",
            "dungeon",
        },
        cows: {
            default: "off",
            "on",
        },
        shops: {
            default: "off",
            "4",
        },
        scrubs: {
            default: "off",
            "affordable",
        },
        skulls: {
            default: "off",
            "dungeons",
        },
        bosskeys: {
            default: "dungeon",
            "anywhere",
        },
        warps: {
            default: "off",
            "on",
        },
        dot: {
            default: "open",
            "closed",
        },
        fountain: {
            default: "closed",
            "open",
        },
        one_major: {
            default: "off",
            "on",
        },
        souls: {
            default: "off",
            "bosses",
        },
        th: {
            default: "off",
            "on",
        },
        boss_er: {
            default: "off",
            "on",
        },
        frogs: {
            default: "off",
            "on",
        },
        bridge: {
            default: "6meds",
            "4meds",
            "5meds",
            "1stones",
            "2stones",
            "3stones",
            "vanilla",
            "5dungeons",
            "6dungeons",
            "7dungeons",
            "8dungeons",
            "9dungeons",
            "1precompleted",
            "2precompleted",
            "3precompleted",
        },
    },
}

steps: [
    A: Ban {
        skippable: false,
    },
    B: Ban {
        skippable: false,
    },
    A: Pick {
        skippable: false,
        defaultable: false,
    },
    B: Pick {
        skippable: false,
        defaultable: false,
    },
    B: Pick {
        skippable: false,
        defaultable: true,
    },
    A: Pick {
        skippable: false,
        defaultable: true,
    },
    A: Pick {
        skippable: false,
        defaultable: false,
    },
    B: Pick {
        skippable: false,
        defaultable: false,
    },
]

settings: {
    "user_message": "Tournoi Francophone Saison 5",
    "reachable_locations": "all",
    "triforce_hunt": match th {
        "on" => true,
        "off" => false,
    },
    "triforce_count_per_world": fr_5_triforce_count_per_world,
    "triforce_goal_per_world": fr_5_triforce_goal_per_world,
    "bridge": match bridge {
        "4meds" | "5meds" | "6meds" => "medallions",
        "1stones" | "2stones" | "3stones" => "stones",
        "5dungeons" | "6dungeons" | "7dungeons" | "8dungeons" | "9dungeons" | "1precompleted" | "2precompleted" | "3precompleted" => "dungeons",
        "vanilla" => "vanilla",
    },
    "bridge_medallions": match bridge {
        "4meds" => 4,
        "5meds" => 5,
        _ => 6,
    },
    "bridge_rewards": match bridge {
        "5dungeons" => 5,
        "6dungeons" => 6,
        "7dungeons" => 7,
        "8dungeons" => 8,
        _ => 9,
    },
    "trials_random": false,
    "trials": 0,
    "shuffle_ganon_bosskey": match bridge {
        "4meds" | "5meds" | "6meds" | "vanilla" => "medallions",
        "1stones" | "2stones" | "3stones" | "5dungeons" | "6dungeons" | "7dungeons" | "8dungeons" | "9dungeons" | "1precompleted" | "2precompleted" | "3precompleted" => "dungeons",
    },
    "ganon_bosskey_rewards": match bridge {
        "5dungeons" => 5,
        "1stones" | "2stones" | "3stones" | "6dungeons" => 6,
        "7dungeons" => 7,
        "8dungeons" => 8,
        _ => 9,
    },
    "open_forest": match deku {
        "closed" => "closed_deku",
        "open" => "open",
    },
    "open_kakariko": "open",
    "open_door_of_time": match dot {
        "closed" => false,
        "open" => true,
    },
    "zora_fountain": fountain,
    "gerudo_fortress": "fast",
    "starting_age": "random",
    "mq_dungeons_mode": "count",
    "mq_dungeons_count": 0,
    "empty_dungeons_mode": match bridge {
        "1precompleted" | "2precompleted" | "3precompleted" => "count",
        _ => "none",
    },
    "empty_dungeons_count": match bridge {
        "1precompleted" => 1,
        "3precompleted" => 2,
        _ => 2,
    },
    "shuffle_interior_entrances": "off",
    "shuffle_grotto_entrances": false,
    "shuffle_dungeon_entrances": match dungeon_er {
        "off" => "off",
        "on" => "simple",
    },
    "shuffle_bosses": match boss_er {
        "off" => "off",
        "on" => "full",
    },
    "mix_entrance_pools": [
        "Interior",
        "GrottoGrave",
    ],
    "owl_drops": match warps {
        "off" => false,
        "on" => true,
    },
    "warp_songs": match warps {
        "off" => false,
        "on" => true,
    },
    "spawn_positions": match start_weirdegg {
        "random-skip" => ["child", "adult"],
        "vanilla-shuffle" => [],
    },
    "free_bombchu_drops": match chubags {
        "off" => false,
        "on" => true,
    },
    "shuffle_song_items": match songs {
        "songs" => "song",
        "anywhere" => "any",
        "dungeon" => "dungeon",
    },
    "shopsanity": shops,
    "tokensanity": skulls,
    "shuffle_scrubs": match scrubs {
        "off" => "off",
        "affordable" => "low",
    },
    "shuffle_child_trade": match start_weirdegg {
        "random-skip" => [],
        "vanilla-shuffle" => ["Weird Egg"],
    },
    "adult_trade_start": [
        "Prescription",
        "Eyeball Frog",
        "Eyedrops",
        "Claim Check",
    ],
    "shuffle_pots": "off",
    "shuffle_crates": "off",
    "shuffle_cows": match cows {
        "off" => false,
        "on" => true,
    },
    "shuffle_ocarinas": match ocarina {
        "startwith" => false,
        "shuffle" => true,
    },
    "shuffle_gerudo_card": match card {
        "vanilla" => false,
        "shuffle" => true,
    },
    "shuffle_frog_song_rupees": match frogs {
        "off" => false,
        "on" => true,
    },
    "shuffle_enemy_spawns": souls,
    "shuffle_mapcompass": "startwith",
    "shuffle_smallkeys": match keysy {
        "off" => "dungeon",
        "on" => "remove",
    },
    "key_rings_choice": "off",
    "shuffle_bosskeys": match bosskeys {
        "dungeon" => "dungeon",
        "anywhere" => "keysanity",
    },
    "enhance_map_compass": match bridge {
        "1precompleted" | "2precompleted" | "3precompleted" => true,
        _ => false,
    },
    "disabled_locations": [
        "Deku Theater Skull Mask",
        "Deku Theater Mask of Truth",
    ],
    "allowed_tricks": [
        "logic_grottos_without_agony",
        "logic_fewer_tunic_requirements",
        "logic_rusted_switches",
        "logic_man_on_roof",
        "logic_windmill_poh",
        "logic_crater_bean_poh_with_hovers",
        "logic_deku_b1_webs_with_bow",
        "logic_dc_scarecrow_gs",
        "logic_dc_jump",
        "logic_lens_botw",
        "logic_child_deadhand",
        "logic_forest_vines",
        "logic_lens_shadow",
        "logic_lens_shadow_platform",
        "logic_lens_bongo",
        "logic_lens_spirit",
        "logic_lens_gtg",
        "logic_lens_castle",
    ],
    "starting_equipment": match start_weirdegg {
        "vanilla-shuffle" => [],
        _ => ["deku_shield"],
    },
    "starting_inventory": match start_weirdegg {
        "random-skip" => match ocarina {
            "startwith" => [
                "ocarina",
                "farores_wind",
                "lens",
                "zeldas_letter",
            ],
            "shuffle" => [
                "farores_wind",
                "lens",
                "zeldas_letter",
            ],
        },
        "vanilla-shuffle" => match ocarina {
            "startwith" => [
                "ocarina",
                "farores_wind",
                "lens",
            ],
            "shuffle" => [
                "farores_wind",
                "lens",
            ],
        },
    },
    "start_with_consumables": match start_weirdegg {
        "random-skip" => true,
        "vanilla-shuffle" => false,
    },
    "start_with_rupees": match shops {
        "off" => false,
        "4" => true,
    },
    "skip_reward_from_rauru": true,
    "no_escape_sequence": true,
    "no_guard_stealth": true,
    "no_epona_race": true,
    "skip_some_minigame_phases": true,
    "free_scarecrow": match ocarina {
        "startwith" => false,
        "shuffle" => true,
    },
    "fast_bunny_hood": true,
    "ruto_already_f1_jabu": true,
    "chicken_count": 0,
    "big_poe_count": 1,
    "hint_dist_user": {
        "name":                  "tournoi_fr",
        "gui_name":              "Tournoi FR",
        "description":           "Hint Distribution for the 5th season of the Francophone Tournament.",
        "add_locations":         [
            { "location": "Sheik in Kakariko", "types": ["always"] },
            { "location": "Deku Tree GS Basement Back Room", "types": ["always"] },
            { "location": "Deku Theater Skull Mask", "types": ["always"] },
        ],
        "remove_locations":      [
            { "location": "Song from Royal Familys Tomb", "types": ["sometimes"] },
            { "location": "Sheik in Forest", "types": ["sometimes"] },
            { "location": "Sheik at Temple", "types": ["sometimes"] },
            { "location": "Sheik in Crater", "types": ["sometimes"] },
            { "location": "Sheik at Colossus", "types": ["sometimes"] },
            { "location": "Ice Cavern Iron Boots Chest", "types": ["sometimes"] },
            { "location": "GF HBA 1500 Points", "types": ["sometimes"] },
            { "location": "GC Maze Left Chest", "types": ["sometimes"] },
            { "location": "GV Chest", "types": ["sometimes"] },
            { "location": "HC Great Fairy Reward", "types": ["sometimes"] },
            { "location": "OGC Great Fairy Reward", "types": ["sometimes"] },
            { "location": "Water Temple River Chest", "types": ["sometimes"] },
            { "location": "Gerudo Training Ground Maze Path Final Chest", "types": ["sometimes"] },
            { "location": "Spirit Temple Silver Gauntlets Chest", "types": ["sometimes"] },
            { "location": "Spirit Temple Mirror Shield Chest", "types": ["sometimes"] },
        ],
        "add_items":             [],
        "remove_items":          [
            { "item": "Zeldas Lullaby", "types": ["goal"] },
            { "item": "Light Arrows", "types": ["goal"] },
        ],
        "dungeons_barren_limit": 1,
        "one_hint_per_goal":     true,
        "named_items_required":  true,
        "vague_named_items":     false,
        "use_default_goals":     true,
        "distribution":          {
            "trial":      {"order":  1, "weight": 0.0, "fixed":   0, "copies": 2},
            "entrance":   {"order":  2, "weight": 0.0, "fixed":   0, "copies": 2},
            "always":     {"order":  3, "weight": 0.0, "fixed":   0, "copies": 2},
            "barren":     {"order":  4, "weight": 0.0, "fixed":   3, "copies": 2},
            "goal":       {"order":  5, "weight": 0.0, "fixed":   5, "copies": 2},
            "sometimes":  {"order":  6, "weight": 0.0, "fixed": 100, "copies": 2},
            "dual":       {"order":  7, "weight": 0.0, "fixed":   0, "copies": 2},
            "random":     {"order":  8, "weight": 9.0, "fixed":   0, "copies": 2},
            "woth":       {"order":  9, "weight": 0.0, "fixed":   0, "copies": 2},
            "item":       {"order":  0, "weight": 0.0, "fixed":   0, "copies": 2},
            "song":       {"order":  0, "weight": 0.0, "fixed":   0, "copies": 2},
            "overworld":  {"order":  0, "weight": 0.0, "fixed":   0, "copies": 2},
            "dungeon":    {"order":  0, "weight": 0.0, "fixed":   0, "copies": 2},
            "junk":       {"order":  0, "weight": 0.0, "fixed":   0, "copies": 2},
            "named-item": {"order":  0, "weight": 0.0, "fixed":   0, "copies": 2},
        },
    },
    "misc_hints": [
        "altar",
        "ganondorf",
        "warp_songs_and_owls",
        "10_skulltulas",
        "20_skulltulas",
        "30_skulltulas",
        "40_skulltulas",
        "50_skulltulas",
    ],
    "correct_chest_appearances": match camc {
        "on" => "both",
        "off" => "off",
    },
    "correct_potcrate_appearances": "textures_content",
    "key_appearance_match_dungeon": true,
    "potcrate_textures_specific": [],
    "blue_fire_arrows": true,
    "one_item_per_dungeon": match one_major {
        "off" => false,
        "on" => true,
    },
    "item_pool_value": "balanced",
    "junk_ice_traps": "off",
    "ice_trap_appearance": "anything",
}
