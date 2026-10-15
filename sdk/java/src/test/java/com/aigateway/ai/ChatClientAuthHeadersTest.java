package com.aigateway.ai;

import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.Test;

import java.lang.reflect.Method;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.time.Duration;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

class ChatClientAuthHeadersTest {

    @Test
    void buildRequestUsesApiKeyHeaderRatherThanBearerAuthorization() throws Exception {
        var apiKey = "gw_test_key";
        var tenantId = "tenant-a";
        var client = new ChatClient(
                HttpClient.newHttpClient(),
                "https://gateway.example",
                apiKey,
                tenantId,
                new ObjectMapper(),
                Duration.ofSeconds(5)
        );
        Method buildRequest = ChatClient.class.getDeclaredMethod(
                "buildRequest", String.class, String.class
        );
        buildRequest.setAccessible(true);
        var request = ((HttpRequest.Builder) buildRequest.invoke(client, "/chat", "{}"))
                .build();

        assertEquals(apiKey, request.headers().firstValue("X-API-Key").orElseThrow());
        assertEquals(tenantId, request.headers().firstValue("X-Tenant-ID").orElseThrow());
        assertEquals("application/json", request.headers().firstValue("Content-Type").orElseThrow());
        assertTrue(request.headers().firstValue("Authorization").isEmpty());
    }
}
