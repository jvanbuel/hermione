//! Multi-tenant building blocks: admin accounts, courses, and membership.

use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use chrono::Utc;
use hermione_entity::{admins, course_admins, courses};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, PaginatorTrait,
    QueryFilter,
};
use uuid::Uuid;

/// The seeded default course (open dev mode + adopted pre-tenancy data).
pub const DEFAULT_COURSE_ID: Uuid = Uuid::from_u128(1);

// --- password hashing ------------------------------------------------------

pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

// --- admins ----------------------------------------------------------------

pub async fn create_admin(
    db: &DatabaseConnection,
    username: &str,
    password: &str,
) -> Result<admins::Model, String> {
    let hash = hash_password(password)?;
    let model = admins::ActiveModel {
        id: Set(Uuid::new_v4()),
        username: Set(username.to_string()),
        password_hash: Set(hash),
        created_at: Set(Utc::now().into()),
    };
    admins::Entity::insert(model)
        .exec_with_returning(db)
        .await
        .map_err(|e| e.to_string())
}

/// Returns the admin id if the credentials are valid.
pub async fn verify_login(db: &DatabaseConnection, username: &str, password: &str) -> Option<Uuid> {
    let admin = admins::Entity::find()
        .filter(admins::Column::Username.eq(username))
        .one(db)
        .await
        .ok()??;
    verify_password(password, &admin.password_hash).then_some(admin.id)
}

pub async fn count_admins(db: &DatabaseConnection) -> u64 {
    admins::Entity::find().count(db).await.unwrap_or(0)
}

pub async fn admin_by_username(db: &DatabaseConnection, username: &str) -> Option<Uuid> {
    admins::Entity::find()
        .filter(admins::Column::Username.eq(username))
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|a| a.id)
}

// --- courses ---------------------------------------------------------------

pub async fn create_course(
    db: &DatabaseConnection,
    slug: &str,
    name: &str,
) -> Result<courses::Model, String> {
    let model = courses::ActiveModel {
        id: Set(Uuid::new_v4()),
        slug: Set(slug.to_string()),
        name: Set(name.to_string()),
        enrollment_token: Set(Uuid::new_v4().simple().to_string()),
        created_at: Set(Utc::now().into()),
    };
    courses::Entity::insert(model)
        .exec_with_returning(db)
        .await
        .map_err(|e| e.to_string())
}

pub async fn course_by_slug(db: &DatabaseConnection, slug: &str) -> Option<courses::Model> {
    courses::Entity::find()
        .filter(courses::Column::Slug.eq(slug))
        .one(db)
        .await
        .ok()
        .flatten()
}

pub async fn course_by_token(db: &DatabaseConnection, token: &str) -> Option<courses::Model> {
    courses::Entity::find()
        .filter(courses::Column::EnrollmentToken.eq(token))
        .one(db)
        .await
        .ok()
        .flatten()
}

// --- membership ------------------------------------------------------------

pub async fn grant_membership(
    db: &DatabaseConnection,
    admin_id: Uuid,
    course_id: Uuid,
) -> Result<(), DbErr> {
    if course_admins::Entity::find_by_id((admin_id, course_id))
        .one(db)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let model = course_admins::ActiveModel {
        admin_id: Set(admin_id),
        course_id: Set(course_id),
    };
    course_admins::Entity::insert(model).exec(db).await?;
    Ok(())
}

pub async fn is_member(db: &DatabaseConnection, admin_id: Uuid, course_id: Uuid) -> bool {
    course_admins::Entity::find_by_id((admin_id, course_id))
        .one(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Courses an admin may access.
pub async fn courses_for_admin(
    db: &DatabaseConnection,
    admin_id: Uuid,
) -> Result<Vec<courses::Model>, DbErr> {
    let course_ids: Vec<Uuid> = course_admins::Entity::find()
        .filter(course_admins::Column::AdminId.eq(admin_id))
        .all(db)
        .await?
        .into_iter()
        .map(|m| m.course_id)
        .collect();
    if course_ids.is_empty() {
        return Ok(Vec::new());
    }
    courses::Entity::find()
        .filter(courses::Column::Id.is_in(course_ids))
        .all(db)
        .await
}

pub async fn all_courses(db: &DatabaseConnection) -> Result<Vec<courses::Model>, DbErr> {
    courses::Entity::find().all(db).await
}
