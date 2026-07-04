import { handleRequest } from './handler';
import express from 'express';

const app = express();
app.get('/', handleRequest);