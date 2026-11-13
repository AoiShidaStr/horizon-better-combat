//! Weapon profiles: which attacks a held item cycles through.
//!
//! Every number here is our own tuning, written from the public description
//! of "weapons swing in an arc and chain a short combo"; nothing is taken
//! from another mod's data files. A category only changes the combo's steps:
//! the game's own attack cooldown and damage still apply underneath, and the
//! step multiplies the damage of the hit the player made.

/// One attack of a combo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attack {
    /// Multiplies the player's attack damage for this step.
    pub damage: f32,
    /// The width the attack would sweep, in degrees: narrow for a stab, wide
    /// for a slash. Kept for the client's feedback and for the day the game
    /// lets a mod hurt more than the clicked target; the server does not use
    /// it yet (see `core.rs`).
    pub arc: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Sword,
    Claymore,
    Dagger,
    Katana,
    Axe,
    Mace,
    Polearm,
    Scythe,
    Tool,
    Fist,
}

#[derive(Debug, PartialEq)]
pub struct Profile {
    pub category: Category,
    pub attacks: &'static [Attack],
}

const fn a(damage: f32, arc: f32) -> Attack {
    Attack { damage, arc }
}

pub static SWORD: Profile = Profile {
    category: Category::Sword,
    attacks: &[a(1.0, 120.0), a(1.0, 120.0), a(1.2, 30.0)],
};
pub static CLAYMORE: Profile = Profile {
    category: Category::Claymore,
    attacks: &[a(1.0, 150.0), a(1.1, 150.0), a(1.3, 160.0)],
};
pub static DAGGER: Profile = Profile {
    category: Category::Dagger,
    attacks: &[a(0.9, 60.0), a(0.9, 60.0), a(1.2, 25.0)],
};
pub static KATANA: Profile = Profile {
    category: Category::Katana,
    attacks: &[a(1.0, 130.0), a(1.1, 130.0), a(1.25, 100.0)],
};
pub static AXE: Profile = Profile {
    category: Category::Axe,
    attacks: &[a(1.0, 90.0), a(1.25, 70.0)],
};
pub static MACE: Profile = Profile {
    category: Category::Mace,
    attacks: &[a(1.0, 80.0), a(1.3, 100.0)],
};
pub static POLEARM: Profile = Profile {
    category: Category::Polearm,
    attacks: &[a(1.0, 20.0), a(1.0, 20.0), a(1.2, 60.0)],
};
pub static SCYTHE: Profile = Profile {
    category: Category::Scythe,
    attacks: &[a(1.0, 170.0), a(1.2, 170.0)],
};
pub static TOOL: Profile = Profile {
    category: Category::Tool,
    attacks: &[a(1.0, 50.0)],
};
pub static FIST: Profile = Profile {
    category: Category::Fist,
    attacks: &[a(1.0, 40.0), a(1.0, 40.0)],
};

/// The item's path without its namespace (`minecraft:iron_sword` -> `iron_sword`).
fn path(item: &str) -> &str {
    item.rsplit_once(':').map_or(item, |(_, p)| p)
}

/// The profile of the held item, or None when the mod leaves the item to the
/// game (the vanilla spears have their own charge and jab).
///
/// `item` is the item id, empty for an empty hand. Only the name is looked
/// at: the server has no helper to ask whether a stack is in an item tag, so
/// a modded weapon with a telling name gets its profile and anything else
/// (a block, food) is a narrow punch.
pub fn classify(item: &str) -> Option<&'static Profile> {
    let name = path(item);
    if name.is_empty() || name == "air" {
        return Some(&FIST);
    }
    let has = |needle: &str| name.contains(needle);
    // The game's own spears: left alone.
    if item.starts_with("minecraft:") && name.ends_with("_spear") {
        return None;
    }
    // Mining and farming tools before "axe", which "pickaxe" contains.
    if has("pickaxe") || has("shovel") || name.ends_with("_hoe") || has("paxel") {
        return Some(&TOOL);
    }
    if has("claymore") || has("greatsword") || has("zweihander") || has("broadsword") {
        return Some(&CLAYMORE);
    }
    if has("dagger") || has("knife") || has("kunai") {
        return Some(&DAGGER);
    }
    if has("katana") || has("rapier") || has("cutlass") || has("saber") || has("sabre") {
        return Some(&KATANA);
    }
    if has("scythe") || has("sickle") {
        return Some(&SCYTHE);
    }
    if has("trident") || has("spear") || has("lance") || has("halberd") || has("glaive") || has("pike") {
        return Some(&POLEARM);
    }
    if has("mace") || has("hammer") || has("club") || has("maul") {
        return Some(&MACE);
    }
    if has("sword") || has("blade") {
        return Some(&SWORD);
    }
    if has("axe") {
        return Some(&AXE);
    }
    // Anything else in the hand (a block, food): a narrow punch.
    Some(&FIST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_items_get_their_category() {
        let cat = |item: &str| classify(item).map(|p| p.category);
        assert_eq!(cat(""), Some(Category::Fist));
        assert_eq!(cat("minecraft:diamond_sword"), Some(Category::Sword));
        assert_eq!(cat("minecraft:netherite_axe"), Some(Category::Axe));
        assert_eq!(cat("minecraft:iron_pickaxe"), Some(Category::Tool));
        assert_eq!(cat("minecraft:stone_shovel"), Some(Category::Tool));
        assert_eq!(cat("minecraft:wooden_hoe"), Some(Category::Tool));
        assert_eq!(cat("minecraft:mace"), Some(Category::Mace));
        assert_eq!(cat("minecraft:trident"), Some(Category::Polearm));
        assert_eq!(cat("minecraft:bread"), Some(Category::Fist));
    }

    #[test]
    fn vanilla_spears_are_left_to_the_game() {
        assert!(classify("minecraft:iron_spear").is_none());
        // Another mod's spear is ours.
        assert_eq!(classify("othermod:bone_spear").map(|p| p.category), Some(Category::Polearm));
    }

    #[test]
    fn modded_names() {
        let cat = |item: &str| classify(item).map(|p| p.category);
        assert_eq!(cat("simplyswords:iron_claymore"), Some(Category::Claymore));
        assert_eq!(cat("x:steel_dagger"), Some(Category::Dagger));
        assert_eq!(cat("x:gold_katana"), Some(Category::Katana));
        assert_eq!(cat("x:war_hammer"), Some(Category::Mace));
        assert_eq!(cat("x:reaper_scythe"), Some(Category::Scythe));
        assert_eq!(cat("x:excalibur_blade"), Some(Category::Sword));
    }

    #[test]
    fn every_profile_has_sane_attacks() {
        for p in [&SWORD, &CLAYMORE, &DAGGER, &KATANA, &AXE, &MACE, &POLEARM, &SCYTHE, &TOOL, &FIST] {
            assert!(!p.attacks.is_empty() && p.attacks.len() <= 255);
            for at in p.attacks {
                assert!(at.damage > 0.0 && at.damage <= 2.0);
                assert!(at.arc > 0.0 && at.arc <= 180.0);
            }
        }
    }
}
