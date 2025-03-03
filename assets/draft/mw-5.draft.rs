// The draft for the 5th Multiworld Tournament (https://midos.house/event/mw/5)

groups: {
    "All Settings": {
        gbk: {
            default: "meds",
            "stones",
            "th",
        },
        bridge: {
            default: "meds",
            "dungeons",
            "vanilla",
        },
        trials: {
            default: "0",
            "2",
        },
        bosskeys: {
            default: "dungeon",
            "regional",
            "vanilla",
        },
        smallkeys: {
            default: "dungeon",
            "regional",
            "vanilla",
        },
        deku: {
            default: "open",
            "closed",
        },
        fountain: {
            default: "closed",
            "open",
        },
        spawn: {
            default: "tot",
            "random",
        },
        dungeon_er: {
            default: "off",
            "on",
        },
        boss_er: {
            default: "off",
            "full",
        },
        warps: {
            default: "off",
            "on",
        },
        chubags: {
            default: "off",
            "on",
        },
        shops: {
            default: "4",
            "off",
        },
        skulls: {
            default: "dungeons",
            "off",
        },
        scrubs: {
            default: "affordable",
            "off",
        },
        cows: {
            default: "off",
            "on",
        },
        card: {
            default: "vanilla",
            "shuffle",
        },
        frogs: {
            default: "off",
            "shuffle",
        },
        camc: {
            default: "both",
            "off",
        },
        hints: {
            default: "path",
            "woth",
        },
    },
}

steps: [
    A: Ban {
        skippable: true,
    },
    B: Ban {
        skippable: true,
    },
    A: Pick {
        skippable: true,
        defaultable: true,
    },
    B: Pick {
        skippable: true,
        defaultable: true,
    },
    B: Pick {
        skippable: true,
        defaultable: true,
    },
    A: Pick {
        skippable: true,
        defaultable: true,
    },
    B: Ban {
        skippable: true,
    },
    A: Ban {
        skippable: true,
    },
    B: Pick {
        skippable: true,
        defaultable: true,
    },
    A: Pick {
        skippable: true,
        defaultable: true,
    },
]

settings: {
    "password_lock": true,
    "user_message": "5th Multiworld Tournament",
    "world_count": 3,
    "triforce_hunt": match gbk {
        "meds" | "stones" => false,
        "th" => true,
    },
    "triforce_count_per_world": 28,
    "triforce_goal_per_world": 24,
    "bridge": match bridge {
        "meds" => "medallions",
        "dungeons" => "dungeons",
        "vanilla" => "vanilla",
    },
    "bridge_rewards": 7,
    "trials": match trials {
        "0" => 0,
        "2" => 2,
    },
    "shuffle_ganon_bosskey": match gbk {
        "stones" => "stones",
        "meds" | "th" => "medallions",
    },
    "shuffle_bosskeys": bosskeys,
    "shuffle_smallkeys": smallkeys,
    "key_rings_choice": match smallkeys {
        "regional" => "all",
        "dungeon" | "vanilla" => "off",
    },
    "shuffle_mapcompass": "startwith",
    "enhance_map_compass": true,
    "open_forest": match deku {
        "open" => "open",
        "closed" => "closed_deku",
    },
    "open_kakariko": "open",
    "open_door_of_time": true,
    "zora_fountain": fountain,
    "gerudo_fortress": "fast",
    "starting_age": match spawn {
        "tot" => "adult",
        "random" => "random",
    },
    "spawn_positions": match spawn {
        "random" => ["child", "adult"],
        "tot" => [],
    },
    "shuffle_dungeon_entrances": match dungeon_er {
        "on" => "simple",
        "off" => "off",
    },
    "shuffle_boss_entrances": boss_er,
    "warp_songs": match warps {
        "on" => true,
        "off" => false,
    },
    "free_bombchu_drops": match chubags {
        "on" => true,
        "off" => false,
    },
    "shopsanity": shops,
    "tokensanity": skulls,
    "shuffle_scrubs": match scrubs {
        "affordable" => "low",
        "off" => "off",
    },
    "shuffle_cows": match cows {
        "on" => true,
        "off" => false,
    },
    "shuffle_gerudo_card": match card {
        "shuffle" => true,
        "vanilla" => false,
    },
    "shuffle_frog_song_rupees": match frogs {
        "shuffle" => true,
        "off" => false,
    },
    "disabled_locations": [
        "Deku Theater Mask of Truth",
        "Kak 40 Gold Skulltula Reward",
        "Kak 50 Gold Skulltula Reward",
    ],
    "allowed_tricks": [
        "logic_fewer_tunic_requirements",
        "logic_grottos_without_agony",
        "logic_child_deadhand",
        "logic_man_on_roof",
        "logic_dc_jump",
        "logic_rusted_switches",
        "logic_windmill_poh",
        "logic_crater_bean_poh_with_hovers",
        "logic_forest_vines",
        "logic_lens_botw",
        "logic_lens_castle",
        "logic_lens_gtg",
        "logic_lens_shadow",
        "logic_lens_shadow_platform",
        "logic_lens_bongo",
        "logic_lens_spirit",
        "logic_visible_collisions",
        "logic_dc_scarecrow_gs",
        "logic_deku_b1_webs_with_bow",
    ],
    "starting_inventory": [
        "ocarina",
        "farores_wind",
        "lens",
        "zeldas_letter",
    ],
    "start_with_consumables": true,
    "start_with_rupees": true,
    "no_escape_sequence": true,
    "no_guard_stealth": true,
    "no_epona_race": true,
    "skip_some_minigame_phases": true,
    "free_scarecrow": true,
    "fast_bunny_hood": true,
    "chicken_count": 3,
    "big_poe_count": 1,
    "ruto_already_f1_jabu": true,
    "fast_shadow_boat": true,
    "correct_chest_appearances": camc,
    "correct_potcrate_appearances": "textures_content",
    "key_appearance_match_dungeon": true,
    "hint_dist": match hints {
        "path" => "mw_path",
        "woth" => "mw_woth",
    },
    "misc_hints": [
        "altar",
        "dampe_diary",
        "ganondorf",
        "warp_songs_and_owls",
        "20_skulltulas",
        "30_skulltulas",
        "frogs2",
    ],
    "blue_fire_arrows": true,
    "tcg_requires_lens": true,
    "junk_ice_traps": "off",
    "ice_trap_appearance": "junk_only",
    "adult_trade_start": [
        "Claim Check",
    ],
}
