import java.util.Properties

plugins {
    alias(libs.plugins.android.application)
}

val signing = Properties().apply {
    rootProject.file("signing.properties").takeIf { it.exists() }?.inputStream()?.use { load(it) }
}

android {
    namespace = "xyz.life2harsh.inkstone"
    compileSdk = 36

    defaultConfig {
        applicationId = "xyz.life2harsh.inkstone"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "1.0"
        manifestPlaceholders["host"] = providers.gradleProperty("inkstone.host").getOrElse("notes.life2harsh.xyz")
        buildConfigField("String", "APP_URL", "\"https://${manifestPlaceholders["host"]}/\"")
    }

    buildFeatures {
        buildConfig = true
    }

    signingConfigs {
        if (signing.isNotEmpty()) {
            create("release") {
                storeFile = rootProject.file(signing.getProperty("storeFile"))
                storePassword = signing.getProperty("storePassword")
                keyAlias = signing.getProperty("keyAlias")
                keyPassword = signing.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
            signingConfig = signingConfigs.findByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(libs.activity.ktx)
    implementation(libs.core.ktx)
    implementation(libs.webkit)
}
